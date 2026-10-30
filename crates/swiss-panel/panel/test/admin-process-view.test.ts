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

/* The Process page (SPEC §process.panel, the library since §panel.pages): the
 * gateway facts row, the apps table with its one-word verbs and its disabled rules,
 * the children grouped by owner, the log follow sheet and the Add/Edit form whose save
 * is the whole-apps PUT. Driven through the real fetch surface and the real library
 * nodes, the way a user sees them. */

import { describe, it, expect, beforeAll, beforeEach, vi } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* eslint-disable-next-line @typescript-eslint/no-explicit-any -- the view is driven by its
   page-registry surface (mount / refresh / countText), so a typed import would add nothing. */
let view: any;
let replies: Record<string, unknown> = {};
let requests: { path: string; method: string; body?: string }[] = [];

const here = dirname(fileURLToPath(import.meta.url));
const $ = (id: string): HTMLElement => document.getElementById(id) as HTMLElement;

/* The shell skeleton from the SERVED index.html (the house idiom): modules in this graph wire
 * shell controls at evaluation time, so the ids exist before the import. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => { return m[1]; });
  return Array.from(new Set(ids)).map((id) => { return '<div id="' + id + '"></div>'; }).join("");
}

function app(o: Partial<{ name: string; status: string; busy: string | null; ports: number[]; pid: number; uptimeSec: number; downPorts: number[]; heldPorts: number[]; lastAction: unknown; autostart: boolean; ui: string | null; stopIsKill: boolean; hasAppLog: boolean; reason: string | null; conflictPid: number; conflictImage: string }>): Record<string, unknown> {
  return Object.assign({
    name: "cc-otel", status: "stopped", ports: [], autostart: false, stopIsKill: true, hasAppLog: false,
  }, o, { busy: o.busy ?? undefined });
}

function serve(apps: unknown[], o: Partial<{ children: unknown[]; config: unknown; revision: number; log: unknown; history: unknown[]; gateway: unknown }> = {}): void {
  replies = {
    "/api/process": { apps, gateway: o.gateway || { pid: 4242, uptimeSec: 3600, memMb: 18 } },
    "/api/process/children": { children: o.children || [] },
    "/api/plugins/process/config": { config: o.config || { apps: [] }, revision: o.revision ?? 7 },
    "/api/process/apps/cc-otel/log": o.log || { source: "capture", path: "logs/apps/cc-otel.log", size: 12, lines: ["line-one", "line-two"], truncated: false, reset: false },
    "/api/process/history": { entries: o.history || [] },
  };
}

function posts(): { path: string; method: string; body?: string }[] { return requests.filter((r) => { return r.method !== "GET"; }); }

function rowNamed(name: string): HTMLElement {
  return Array.from(document.querySelectorAll<HTMLElement>("#pane [data-prrow]")).find((n) => { return n.dataset.prrow === name; }) as HTMLElement;
}

function menuItem(label: string): HTMLElement {
  return Array.from(document.querySelectorAll<HTMLElement>("#menu button")).find((b) => { return b.textContent === label; }) as HTMLElement;
}

beforeAll(async () => {
  Object.assign(globalThis, {
    fetch: (url: string, init?: RequestInit): Promise<Response> => {
      const u = String(url);
      const method = init?.method || "GET";
      requests.push({ path: u, method, body: init?.body as string | undefined });
      let reply = replies[(method !== "GET" ? method + " " : "") + u];
      if (reply === undefined) {
        // Path-shaped keys (a log query, a start on a named app) fall back to their stem.
        const stem = u.split("?")[0];
        for (const key of Object.keys(replies)) {
          if (key.split("?")[0] === stem && !key.startsWith(method + " ")) reply = replies[key];
        }
      }
      const ok = reply !== undefined;
      return Promise.resolve({ status: ok ? 200 : 500, ok, json: () => { return Promise.resolve(reply ?? { error: "no stub for " + u }); } } as Response);
    },
    confirm: () => { return true; },
  });
  document.body.innerHTML = shellSkeleton();
  view = await import("../src/views/process.js");
});

beforeEach(() => {
  requests = [];
  document.body.innerHTML = shellSkeleton();
  localStorage.clear();
  vi.restoreAllMocks();
});

describe("the Process page (process plugin, SPEC §process.panel)", () => {
  it("the view module exists at the entry the page descriptor points at", () => {
    expect(existsSync(join(here, "..", "src", "views", "process.ts"))).toBe(true);
  });

  it("the head is one sentence, the gateway's own facts under it, and the one primary", async () => {
    serve([]);
    await view.mount();
    const pane = $("pane");
    expect(pane.querySelector(".pane-desc")?.textContent).toBe("Manage local apps and inspect gateway-owned processes.");
    expect(pane.querySelector(".pane-sub")?.textContent).toContain("4242");
    const add = pane.querySelector("#prAdd");
    expect(add?.className).toContain("primary");
    expect($("countChip").textContent).toBe("0 apps");
    view.unmount();
  });

  it("an empty table is the empty state whose action opens the Add sheet", async () => {
    serve([]);
    await view.mount();
    const act = document.querySelector<HTMLElement>("[data-empty-action]");
    expect(act?.textContent).toBe("Add app");
    act!.click();
    await new Promise((r) => { return setTimeout(r, 0); });
    expect($("sheet").hidden).toBe(false);
    expect(document.getElementById("pr-f-name")).toBeTruthy();
    view.unmount();
  });

  it("a row carries the badge, ports, pid, uptime and the two marks; a start that failed while the app healed reads as reconciliation", async () => {
    serve([
      app({
        status: "running", ports: [4317, 4318], pid: 999,
        lastAction: { action: "start", ok: false, message: "exited 1; see the capture", at: Date.now() - 5000 },
        autostart: true, stopIsKill: false,
      }),
      // The same failed start on a stopped row is the explanation of the
      // state, so there it stays the red line.
      app({ name: "gone", status: "stopped", ports: [4319], lastAction: { action: "start", ok: false, message: "exited 3", at: Date.now() - 4000 } }),
      // A failed stop on a running row is current and actionable: red stays.
      app({ name: "stuck", status: "running", ports: [4320], pid: 77, lastAction: { action: "stop", ok: false, message: "the stop command overran", at: Date.now() - 3000 } }),
    ]);
    await view.mount();
    const r = rowNamed("cc-otel");
    expect(r.querySelector(".lrow-name")?.textContent).toBe("cc-otel");
    // The launcher pattern: the start failed, its child holds the port now -
    // the raw sentence on a running row would read as a contradiction.
    expect(r.querySelector(".lrow-err")).toBe(null);
    expect(r.querySelector(".lrow-sub")?.textContent).toContain("holds the port now");
    const facts = r.querySelector(".lrow-meta")?.textContent;
    expect(Array.from(r.querySelectorAll(".pchip")).map((c) => { return c.textContent; })).toEqual(["4317", "4318"]);
    expect(facts).toContain("PID 999");
    const tags = Array.from(r.querySelectorAll(".tag")).map((t) => { return t.textContent; });
    expect(tags).toContain("running");
    expect(tags).toContain("autostart");
    expect(tags).not.toContain("stop = kill");
    expect(rowNamed("gone").querySelector(".lrow-err")?.textContent).toContain("exited 3");
    expect(rowNamed("stuck").querySelector(".lrow-err")?.textContent).toContain("overran");
    view.unmount();
  });

  it("a button the status makes meaningless is disabled; a busy row shows the action's name", async () => {
    serve([
      app({ name: "up", status: "running" }),
      app({ name: "clash", status: "conflict", reason: "port 4317 is held by node.exe (pid 5)", conflictPid: 5, conflictImage: "node.exe" }),
      app({ name: "gone", status: "stopped" }),
      app({ name: "busy-one", status: "stopped", busy: "start" }),
    ]);
    await view.mount();
    const primary = (name: string): HTMLButtonElement => { return rowNamed(name).querySelector("[data-pract]") as HTMLButtonElement; };
    expect(primary("up").textContent).toBe("Stop");
    expect(primary("up").disabled).toBe(false);
    expect(primary("clash").disabled).toBe(true);
    expect(primary("gone").textContent).toBe("Start");
    expect(primary("gone").disabled).toBe(false);
    expect(primary("busy-one").disabled).toBe(true);
    expect(rowNamed("busy-one").querySelector(".lrow-col .tag")?.textContent).toBe("Starting…");
    view.unmount();
  });

  it("the one-word verb POSTs its route, and the menu holds the rest, Force only after canForce", async () => {
    serve([app({ status: "stopped", ports: [4317], pid: 0 })]);
    await view.mount();
    (rowNamed("cc-otel").querySelector("[data-pract]") as HTMLButtonElement).click();
    const start = posts().find((p) => { return p.path.endsWith("/cc-otel/start"); });
    expect(start?.method + " " + start?.path).toBe("POST /api/process/apps/cc-otel/start");
    expect(start?.body).toBe("{}"); // an absent body is a 400 sentence, not an empty action
    view.unmount();

    serve([app({ status: "stopped", ports: [4317], lastAction: { action: "stop", ok: false, message: "the stop command left it running; force is available", at: Date.now(), canForce: true } })]);
    await view.mount();
    (rowNamed("cc-otel").querySelector("[data-prmore]") as HTMLElement).click();
    expect(menuItem("Force stop")).toBeTruthy();
    expect(menuItem("Restart")).toBeTruthy();
    menuItem("Force stop").click();
    expect(posts().map((p) => { return p.path; })).toContain("/api/process/apps/cc-otel/stop");
    expect(JSON.parse(posts().filter((p) => { return p.path.endsWith("/stop"); })[0].body || "{}")).toEqual({ force: true });
    view.unmount();

    serve([app({ status: "stopped", ports: [4317], lastAction: { action: "start", ok: true, message: "up", at: Date.now() } })]);
    await view.mount();
    (rowNamed("cc-otel").querySelector("[data-prmore]") as HTMLElement).click();
    expect(menuItem("Force stop")).toBeFalsy();
    view.unmount();
  });

  it("the logs sheet: both doors when the app has its own log, the lines, and the history under them", async () => {
    serve([app({ status: "stopped", hasAppLog: true })], {
      history: [{ ts: Date.now() - 60000, app: "cc-otel", action: "start", by: "panel", ok: false, message: "exited 3" }],
    });
    await view.mount();
    (rowNamed("cc-otel").querySelector("[data-prmore]") as HTMLElement).click();
    menuItem("Logs").click();
    await new Promise((r) => { return setTimeout(r, 0); });
    expect($("sheet").hidden).toBe(false);
    const tabs = Array.from(document.querySelectorAll<HTMLElement>("[data-prlogsrc]"));
    expect(tabs.map((t) => { return t.dataset.prlogsrc; })).toEqual(["capture", "app"]);
    expect(document.getElementById("sheet")?.textContent).toContain("line-one");
    expect(document.getElementById("sheet")?.textContent).toContain("exited 3");
    (tabs[1] as HTMLElement).click();
    await new Promise((r) => { return setTimeout(r, 0); });
    expect(requests.some((r2) => { return r2.path.includes("source=app"); })).toBe(true);
    view.unmount();
  });

  it("the Add sheet saves the whole apps array with the revision it read; a 400 sentence lands under the form", async () => {
    serve([], { config: { apps: [] }, revision: 7 });
    await view.mount();
    (document.querySelector("#prAdd") as HTMLElement).click();
    await new Promise((r) => { return setTimeout(r, 0); });
    ($("pr-f-name") as HTMLInputElement).value = "cc-otel";
    ($("pr-f-program") as HTMLInputElement).value = "node";
    ($("pr-f-args") as HTMLTextAreaElement).value = "server.js\n--port 4317";
    ($("pr-f-ports") as HTMLInputElement).value = "4317";
    ($("pr-f-image") as HTMLInputElement).value = "node.exe";
    ($("pr-f-env") as HTMLTextAreaElement).value = "OTEL_PORT=4317";
    replies["PUT /api/plugins/process/config"] = { error: "invalid config: apps[0].dir must be absolute" };
    const put = replies["PUT /api/plugins/process/config"];
    void put;
    const realFetch = globalThis.fetch;
    globalThis.fetch = ((url: string, init?: RequestInit) => {
      if (String(url) === "/api/plugins/process/config" && init?.method === "PUT") {
        requests.push({ path: String(url), method: "PUT", body: init.body as string });
        return Promise.resolve({ status: 400, ok: false, json: () => { return Promise.resolve({ error: "invalid config: apps[0].dir must be absolute" }); } } as Response);
      }
      return (realFetch as (u: string, i?: RequestInit) => Promise<Response>)(url, init);
    }) as typeof fetch;
    (document.getElementById("prFormSave") as HTMLElement).click();
    await new Promise((r) => { return setTimeout(r, 0); });
    globalThis.fetch = realFetch;
    const put2 = posts().find((p) => { return p.method === "PUT"; });
    expect(put2).toBeTruthy();
    const body = JSON.parse(put2!.body || "{}");
    expect(body.revision).toBe(7);
    expect(body.config.apps[0].name).toBe("cc-otel");
    expect(body.config.apps[0].start.args).toEqual(["server.js", "--port 4317"]);
    expect(body.config.apps[0].image).toBe("node.exe");
    expect(body.config.apps[0].env).toEqual({ OTEL_PORT: "4317" });
    expect(document.getElementById("pr-f-err")?.textContent).toContain("must be absolute");
    expect(document.getElementById("pr-f-err")?.hidden).toBe(false);
    view.unmount();
  });

  it("children are grouped by owner and a stoppable row stops through the coordinator", async () => {
    serve([], { children: [
      { pid: 11, owner: "mcp", kind: "mcp", label: "figma", uptimeSec: 90, memMb: 40, stoppable: true },
      { pid: 12, owner: "run", kind: "run", label: "build · nightly", uptimeSec: 30, stoppable: false },
    ] });
    await view.mount();
    const bands = Array.from(document.querySelectorAll<HTMLElement>("#pane .grp-name")).map((n) => { return n.textContent; });
    expect(bands).toEqual(["mcp", "run"]);
    const stopBtn = document.querySelector("[data-prstopchild='11']") as HTMLButtonElement;
    expect(stopBtn).toBeTruthy();
    expect(document.querySelector("[data-prstopchild='12']")).toBeNull();
    stopBtn.click();
    await new Promise((r) => { return setTimeout(r, 0); });
    expect(posts().map((p) => { return p.path; })).toContain("/api/process/children/11/stop");
    view.unmount();
  });

  it("leaving the page stops every poll: unmount clears the timers and the listener", async () => {
    serve([]);
    await view.mount();
    expect($("pane").onclick).toBeTruthy();
    view.unmount();
    expect($("pane").onclick).toBeNull();
  });

  it("a follow tick appends to the SAME pre - the sheet is never rebuilt under the reader", async () => {
    serve([app({ status: "running", pid: 9 })]);
    await view.mount();
    (rowNamed("cc-otel").querySelector("[data-prmore]") as HTMLElement).click();
    menuItem("Logs").click();
    await new Promise((r) => { return setTimeout(r, 0); });
    const pre = document.getElementById("prLogPre") as HTMLElement;
    expect(pre).toBeTruthy();
    // Tag the live nodes: a rebuild would replace them whatever the words say.
    (pre as unknown as { __tag: number }).__tag = 1;
    const seg = document.querySelector("[data-prlogsrc='capture']") as HTMLElement;
    (seg as unknown as { __tag: number }).__tag = 2;
    replies["/api/process/apps/cc-otel/log"] = { source: "capture", path: "logs/apps/cc-otel.log", size: 20, lines: ["line-three"], truncated: false, reset: false };
    vi.useFakeTimers();
    ($("prFollow") as HTMLElement).click();
    await vi.advanceTimersByTimeAsync(1100);
    vi.useRealTimers();
    const after = document.getElementById("prLogPre") as HTMLElement;
    expect((after as unknown as { __tag?: number }).__tag).toBe(1);
    expect(after.textContent).toContain("line-three");
    const segAfter = document.querySelector("[data-prlogsrc='capture']") as HTMLElement;
    expect((segAfter as unknown as { __tag?: number }).__tag).toBe(2);
    view.unmount();
  });

  it("Edit pre-fills from the config read AT OPEN - the first Edit of a session carries the real program", async () => {
    serve([app({ status: "stopped", ports: [4317, 4318] })], {
      config: { apps: [{ name: "cc-otel", start: { program: "C:\\bin\\cc-otel.exe", args: ["serve"] }, ports: [4317, 4318], image: "cc-otel.exe" }] },
    });
    await view.mount();
    (rowNamed("cc-otel").querySelector("[data-prmore]") as HTMLElement).click();
    menuItem("Edit").click();
    await new Promise((r) => { return setTimeout(r, 0); });
    expect(($("pr-f-program") as HTMLInputElement).value).toBe("C:\\bin\\cc-otel.exe");
    expect(($("pr-f-args") as HTMLTextAreaElement).value).toBe("serve");
    expect(($("pr-f-ports") as HTMLInputElement).value).toBe("4317, 4318");
    view.unmount();
  });

  it("occupancy first: the pid's held ports are the chips, a declared-but-missing one rides red (a stopped app's stay quiet gray)", async () => {
    serve([
      app({ name: "up-app", status: "running", ports: [4317, 4318], heldPorts: [4317, 4318, 9229], pid: 5 }),
      app({ name: "part-app", status: "partial", ports: [4317, 4318], heldPorts: [4317], pid: 6 }),
      app({ name: "off-app", status: "stopped", ports: [14317] }),
    ]);
    await view.mount();
    const tone = (c: HTMLElement): string => {
      return c.classList.contains("up") ? "up" : c.classList.contains("down") ? "down" : "held";
    };
    const chips = (name: string): string[] => {
      return Array.from(rowNamed(name).querySelectorAll<HTMLElement>(".pchip")).map((c) => {
        return c.textContent + ":" + tone(c);
      });
    };
    // Held leads, declared or not: 9229 is the pid's own fact.
    expect(chips("up-app")).toEqual(["4317:up", "4318:up", "9229:up"]);
    const heldTitle = rowNamed("up-app").querySelectorAll<HTMLElement>(".pchip")[2].title;
    expect(heldTitle).toContain("not declared");
    // A promise the running app has not kept rides red after the held set.
    expect(chips("part-app")).toEqual(["4317:up", "4318:down"]);
    // No pid holds anything: the declared ports are quiet gray, never red.
    expect(chips("off-app")).toEqual(["14317:held"]);
    view.unmount();
  });

  it("the Program picker: Browse lists the folder, a folder walks in, an executable fills Program and closes", async () => {
    serve([]);
    replies["/api/process/fs"] = { dir: "C:\\tools", parent: "C:\\", truncated: false, entries: [
      { name: "bin", dir: true, exe: false },
      { name: "cc-otel.exe", dir: false, exe: true },
      { name: "readme.txt", dir: false, exe: false },
    ] };
    await view.mount();
    ($("prAdd") as HTMLElement).click();
    await new Promise((r) => { return setTimeout(r, 0); });
    ($("pr-f-browse") as HTMLElement).click();
    await new Promise((r) => { return setTimeout(r, 0); });
    expect($("prPick").hidden).toBe(false);
    const rows = Array.from($("prPickList").querySelectorAll<HTMLElement>(".dirpick-row"));
    expect(rows.map((r2) => { return (r2.querySelector(".dirpick-name") as HTMLElement).textContent; })).toEqual(["bin", "cc-otel.exe", "readme.txt"]);
    expect(requests.some((r2) => { return r2.path.includes("/api/process/fs"); })).toBe(true);
    // The typed program's own directory is where the listing starts.
    ($("pr-f-program") as HTMLInputElement).value = "C:\\elsewhere\\tool.exe";
    ($("pr-f-browse") as HTMLElement).click(); // toggle shut, then open from the new dir
    await new Promise((r) => { return setTimeout(r, 0); });
    ($("pr-f-browse") as HTMLElement).click();
    await new Promise((r) => { return setTimeout(r, 0); });
    expect(requests.some((r2) => { return r2.path.includes(encodeURIComponent("C:\\elsewhere")); })).toBe(true);
    (rows[1] as HTMLElement).click();
    expect(($("pr-f-program") as HTMLInputElement).value).toBe("C:\\tools\\cc-otel.exe");
    expect($("prPick").hidden).toBe(true);
    view.unmount();
  });

describe("SPEC §process.panel - the capture is events, not a stream", () => {
  it("every start/stop is its own disclosure; the newest run is open, the rest fold", async () => {
    serve([app({ status: "stopped" })]);
    replies["/api/process/apps/cc-otel/log"] = {
      source: "capture", path: "logs/apps/cc-otel.log", size: 10, truncated: false, reset: false,
      lines: ["--- 2026-10-09T01:00:00Z start by panel ---", "hello", "--- 2026-10-09T01:05:00Z stop by panel ---", "bye"],
    };
    await view.mount();
    (rowNamed("cc-otel").querySelector("[data-prmore]") as HTMLElement).click();
    menuItem("Logs").click();
    await new Promise((r) => { return setTimeout(r, 0); });
    const evts = Array.from(document.querySelectorAll("#sheet details.lrow-disc"));
    expect(evts.length).toBe(2);
    expect((evts[0] as HTMLDetailsElement).open).toBe(false);
    expect((evts[1] as HTMLDetailsElement).open).toBe(true);
    expect((document.getElementById("prLogPre") as HTMLElement).textContent).toContain("bye");
    // A tick carrying a new separator births a NEW event row, older stays folded.
    replies["/api/process/apps/cc-otel/log"] = {
      source: "capture", path: "logs/apps/cc-otel.log", size: 20, truncated: false, reset: false,
      lines: ["--- 2026-10-09T01:09:00Z start by panel ---", "again"],
    };
    vi.useFakeTimers();
    ($("prFollow") as HTMLElement).click();
    await vi.advanceTimersByTimeAsync(1100);
    vi.useRealTimers();
    const after = Array.from(document.querySelectorAll("#sheet details.lrow-disc"));
    expect(after.length).toBe(3);
    expect((after[2] as HTMLDetailsElement).open).toBe(true);
    expect((after[1] as HTMLDetailsElement).open).toBe(false);
    expect((document.getElementById("prLogPre") as HTMLElement).textContent).toContain("again");
    view.unmount();
  });

  it("clicking the ACTIVE source tab re-reads the tail - a click always answers", async () => {
    serve([app({ status: "stopped" })]);
    await view.mount();
    (rowNamed("cc-otel").querySelector("[data-prmore]") as HTMLElement).click();
    menuItem("Logs").click();
    await new Promise((r) => { return setTimeout(r, 0); });
    const reads = (): number => { return requests.filter((q) => { return q.path.includes("/log?"); }).length; };
    const before = reads();
    (document.querySelector("[data-prlogsrc='capture']") as HTMLElement).click();
    await new Promise((r) => { return setTimeout(r, 0); });
    expect(reads()).toBe(before + 1);
    expect(document.getElementById("prLogPre")).toBeTruthy();
    view.unmount();
  });

  it("history reads as runs: a start, its stop and its exit share one row; raw sentences fold into its detail", async () => {
    serve([app({ status: "stopped" })], { history: [
      { ts: 1, app: "cc-otel", action: "start", by: "panel", ok: true, message: "started (pid 8552)" },
      { ts: 3000, app: "cc-otel", action: "stop", by: "panel", ok: true, message: "killed 8552" },
      { ts: 3000, app: "cc-otel", action: "exit", by: "plugin", ok: true, message: "exited with code 1", exitCode: 1 },
      { ts: 9000, app: "cc-otel", action: "start", by: "cli", ok: true, message: "started; listening" },
    ] });
    await view.mount();
    (rowNamed("cc-otel").querySelector("[data-prmore]") as HTMLElement).click();
    menuItem("Logs").click();
    await new Promise((r) => { return setTimeout(r, 0); });
    const rowsHist = Array.from(document.querySelectorAll("#prHistBlock .lrow"));
    // Two runs: the closed one (start+stop+exit), the still-running one.
    expect(rowsHist.length).toBe(2);
    const first = rowsHist[0] as HTMLElement;
    expect(first.textContent).toContain("stopped by panel");
    expect(first.textContent).toContain("exit 1");
    // The raw sentences live in the folded detail, not on the line.
    expect((first.querySelector(".lrow-sub") as HTMLElement).textContent).not.toContain("started (pid 8552)");
    view.unmount();
  });

  it("a follow tick on an event-split capture appends to the NEWEST run only - never re-dumps the whole buffer", async () => {
    serve([app({ status: "running", pid: 9 })]);
    replies["/api/process/apps/cc-otel/log"] = {
      source: "capture", path: "logs/apps/cc-otel.log", size: 3,
      lines: ["--- 2026-10-09T01:00:00Z start by panel ---", "old run line", "--- 2026-10-09T02:00:00Z start by panel ---", "fresh line"],
      truncated: false, reset: false,
    };
    await view.mount();
    (rowNamed("cc-otel").querySelector("[data-prmore]") as HTMLElement).click();
    menuItem("Logs").click();
    await new Promise((r) => { return setTimeout(r, 0); });
    const discs = Array.from(document.querySelectorAll("#prLogBlock details.lrow-disc"));
    expect(discs.length).toBe(2);
    // a tick with NO separator takes the append-only fast path
    replies["/api/process/apps/cc-otel/log"] = {
      source: "capture", path: "logs/apps/cc-otel.log", size: 5, truncated: false, reset: false,
      lines: ["tick line"],
    };
    vi.useFakeTimers();
    ($("prFollow") as HTMLElement).click();
    await vi.advanceTimersByTimeAsync(1100);
    vi.useRealTimers();
    const pre = document.getElementById("prLogPre") as HTMLElement;
    expect(pre.textContent).toContain("fresh line");
    expect(pre.textContent).toContain("tick line");
    // the whole-buffer re-dump would surface the raw separator line and the OLD run inside the newest one
    expect(pre.textContent).not.toContain("--- 2026-10-09T01:00:00Z start by panel ---");
    expect(pre.textContent).not.toContain("old run line");
    view.unmount();
  });

  it("a FAILED start is its own red run - never a green still-running; its early exit line joins it", async () => {
    serve([app({ status: "stopped" })], { history: [
      { ts: 1000, app: "cc-otel", action: "exit", by: "plugin", ok: true, message: "exited with code 2", exitCode: 2 },
      { ts: 1184, app: "cc-otel", action: "start", by: "panel", ok: false, message: "could not start C:/definitely/missing.exe (os error 3)" },
    ] });
    await view.mount();
    (rowNamed("cc-otel").querySelector("[data-prmore]") as HTMLElement).click();
    menuItem("Logs").click();
    await new Promise((r) => { return setTimeout(r, 0); });
    const runs = Array.from(document.querySelectorAll("#prHistBlock .lrow"));
    expect(runs.length).toBe(1);
    const first = runs[0] as HTMLElement;
    expect(first.textContent).toContain("did not start:");
    expect(first.textContent).toContain("exit 2");
    expect(first.textContent).not.toContain("still running");
    expect(first.querySelector(".dot.down")).toBeTruthy();
    view.unmount();
  });
});

  it("the shell AbortSignal disarms local and exported polling", async () => {
    serve([app({})]);
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    vi.useFakeTimers();
    const controller = new AbortController();
    await view.mount({ signal: controller.signal });
    controller.abort();
    const reads = fetchSpy.mock.calls.length;
    $("pane").textContent = "another page";
    try {
      await vi.advanceTimersByTimeAsync(6000);
      await view.poll();
      expect(fetchSpy.mock.calls.length).toBe(reads);
      expect($("pane").textContent).toBe("another page");
    } finally { view.unmount(); vi.useRealTimers(); }
  });

  it("hidden documents suspend both inventory schedules", async () => {
    serve([app({})]);
    const fetchSpy = vi.spyOn(globalThis, "fetch");
    vi.useFakeTimers();
    await view.mount();
    const hidden = vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    const reads = fetchSpy.mock.calls.length;
    try {
      await vi.advanceTimersByTimeAsync(6000);
      await view.poll();
      expect(fetchSpy.mock.calls.length).toBe(reads);
    } finally { hidden.mockRestore(); view.unmount(); vi.useRealTimers(); }
  });

  it("memory-only inventory updates preserve focused row controls", async () => {
    serve([app({ status: "running", pid: 9 })]);
    await view.mount();
    const button = rowNamed("cc-otel").querySelector<HTMLButtonElement>("[data-pract]");
    button!.focus();
    replies["/api/process"] = { apps: [app({ status: "running", pid: 9 })], gateway: { pid: 4242, uptimeSec: 3600, memMb: 25 } };
    await view.poll();
    expect(rowNamed("cc-otel").querySelector("[data-pract]")).toBe(button);
    expect(document.activeElement).toBe(button);
    expect($("prGateway").textContent).toContain("25 MB");
    view.unmount();
  });

  it("an uptime tick patches the row's own text - the focused control keeps its node; a status change still replaces the row", async () => {
    serve([app({ status: "running", ports: [4317], pid: 9, uptimeSec: 60 })]);
    await view.mount();
    const rowNode = rowNamed("cc-otel");
    (rowNode as unknown as { __tag: number }).__tag = 7;
    const stop = rowNode.querySelector<HTMLButtonElement>("[data-pract]") as HTMLButtonElement;
    stop.focus();
    expect(document.activeElement).toBe(stop);
    expect(rowNode.querySelector("[data-pruptime]")?.textContent).toContain("60 sec");
    replies["/api/process"] = { apps: [app({ status: "running", ports: [4317], pid: 9, uptimeSec: 130 })], gateway: { pid: 4242, uptimeSec: 3600, memMb: 18 } };
    await view.poll();
    // The row is the SAME element, still in the document, focus intact - only
    // its uptime words changed, so nothing under the keyboard was replaced.
    const kept = rowNamed("cc-otel");
    expect(kept).toBe(rowNode);
    expect((kept as unknown as { __tag?: number }).__tag).toBe(7);
    expect(document.activeElement).toBe(stop);
    expect(kept.querySelector("[data-pruptime]")?.textContent).toContain("2 min");
    // A structural change (the status) still replaces the row.
    replies["/api/process"] = { apps: [app({ status: "stopped", ports: [4317], pid: 9 })], gateway: { pid: 4242, uptimeSec: 3600, memMb: 18 } };
    await view.poll();
    expect(rowNamed("cc-otel")).not.toBe(rowNode);
    view.unmount();
  });

  it("an unknown app still shows the failed action instead of hiding it behind generic status prose", async () => {
    serve([app({ status: "unknown", reason: "not started by this swiss; declare ports", lastAction: { action: "start", ok: false, message: "program was not found", at: 1 } })]);
    await view.mount();
    try {
      expect(rowNamed("cc-otel").querySelector(".lrow-err")?.textContent).toContain("program was not found");
      expect(rowNamed("cc-otel").querySelector(".lrow-col .tag")?.getAttribute("title")).toContain("declare ports");
    } finally { view.unmount(); }
  });

  it("current conflict beats an old successful action and logs has a direct door", async () => {
    serve([app({ status: "conflict", reason: "held by stranger", lastAction: { action: "start", ok: true, message: "old success", at: 1 } })]);
    await view.mount();
    expect(rowNamed("cc-otel").querySelector(".lrow-sub")?.textContent).toBe("held by stranger");
    const log = rowNamed("cc-otel").querySelector<HTMLButtonElement>("[data-prlogs]");
    expect(log?.getAttribute("aria-label")).toBe("Logs for cc-otel");
    log!.click();
    await new Promise((r) => { return setTimeout(r, 0); });
    expect($("sheet").hidden).toBe(false);
    view.unmount();
  });

  it("a child-only poll paints without waiting for an inventory response", async () => {
    serve([]);
    vi.useFakeTimers();
    await view.mount();
    replies["/api/process/children"] = { children: [{ pid: 51, owner: "terminal", kind: "terminal", label: "new shell", uptimeSec: 1, stoppable: false }] };
    try {
      await vi.advanceTimersByTimeAsync(5000);
      expect(document.querySelector("[data-prchild='51']")?.textContent).toContain("new shell");
    } finally { view.unmount(); vi.useRealTimers(); }
  });

  it("a late mount response cannot repaint or restart polling after leaving", async () => {
    serve([]);
    const original = globalThis.fetch;
    let finish: (r: Response) => void = () => {};
    vi.spyOn(globalThis, "fetch").mockImplementation((url, init) => {
      if (String(url) === "/api/process") return new Promise<Response>((resolve) => { finish = resolve; });
      return original(url, init);
    });
    const mounting = view.mount();
    view.unmount();
    $("pane").textContent = "another page";
    finish({ ok: true, status: 200, json: async () => ({ apps: [app({})] }) } as Response);
    await mounting;
    try {
      expect($("pane").textContent).toBe("another page");
      expect($("pane").onclick).toBeNull();
    } finally { view.unmount(); }
  });

  it("overlapping shell and local polls share one in-flight inventory request", async () => {
    serve([app({})]);
    await view.mount();
    const original = globalThis.fetch;
    let reads = 0;
    const finishes: ((r: Response) => void)[] = [];
    vi.spyOn(globalThis, "fetch").mockImplementation((url, init) => {
      if (String(url) === "/api/process") {
        reads++;
        return new Promise<Response>((resolve) => { finishes.push(resolve); });
      }
      return original(url, init);
    });
    const a = view.poll();
    const b = view.poll();
    for (const finish of finishes) finish({ ok: true, status: 200, json: async () => ({ apps: [app({})] }) } as Response);
    await Promise.all([a, b]);
    try { expect(reads).toBe(1); } finally { view.unmount(); }
  });

  it("a changed reason repaints the affected row even when its verdict stays unknown", async () => {
    serve([app({ status: "unknown", reason: "first reason" })]);
    await view.mount();
    replies["/api/process"] = { apps: [app({ status: "unknown", reason: "new reason" })], gateway: { pid: 4242, uptimeSec: 3600, memMb: 18 } };
    await view.poll();
    try { expect(rowNamed("cc-otel").textContent).toContain("new reason"); } finally { view.unmount(); }
  });

  it("gateway child groups fold and retain their state after refresh", async () => {
    serve([], { children: [{ pid: 11, owner: "mcp", kind: "mcp", label: "figma", uptimeSec: 90, stoppable: true }] });
    await view.mount();
    const toggle = document.querySelector<HTMLButtonElement>("#pane .grp-toggle");
    toggle!.click();
    try {
      expect(toggle!.getAttribute("aria-expanded")).toBe("false");
      await view.refresh();
      expect(document.querySelector("#pane .grp-toggle")?.getAttribute("aria-expanded")).toBe("false");
    } finally { view.unmount(); }
  });

});
