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

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as logs from "../src/views/process/logs.js";
import type { HistoryEntry, LogAnswer, ProcessRow } from "../src/views/process/types.js";
import { closeSheet, sheet, showSheet } from "../src/ui/sheet.js";

const app: ProcessRow = { name: "demo", status: "running", ports: [], autostart: false, stopIsKill: true, hasAppLog: true };
const header = "--- 2026-10-09T01:00:00Z start by panel ---";
let requests: string[];
let history: HistoryEntry[];
let reply: (url: string) => Response | Promise<Response>;

function response(body: unknown, status = 200): Response {
  return { status, ok: status >= 200 && status < 300, json: () => { return Promise.resolve(body); } } as Response;
}
function answer(lines: string[], extra: Partial<LogAnswer> = {}): LogAnswer {
  return { source: "capture", path: "logs/apps/demo.log", size: 100, lines, truncated: false, reset: false, ...extra };
}
function node(id: string): HTMLElement { return document.getElementById(id) as HTMLElement; }
function source(id: string): HTMLElement { return document.querySelector('[data-prlogsrc="' + id + '"]') as HTMLElement; }
function deferred(): { promise: Promise<Response>; resolve: (r: Response) => void } {
  let resolve!: (r: Response) => void;
  return { promise: new Promise<Response>((done) => { resolve = done; }), resolve: (r) => { resolve(r); } };
}
async function settle(): Promise<void> { await vi.advanceTimersByTimeAsync(0); }
async function tick(): Promise<void> { await vi.advanceTimersByTimeAsync(1000); }
function beginFollow(): void { node("prFollow").click(); }
function logReads(): string[] { return requests.filter((url) => { return url.includes("/log?"); }); }
function historyReads(): string[] { return requests.filter((url) => { return url.includes("/history?"); }); }

beforeEach(() => {
  vi.useFakeTimers();
  document.body.innerHTML = '<div id="sheet" hidden></div><div id="toast" hidden></div>';
  requests = [];
  history = [];
  reply = (url) => { return response(url.includes("/history?") ? { entries: history } : answer(["initial"])); };
  vi.stubGlobal("fetch", vi.fn((url: string) => { requests.push(String(url)); return Promise.resolve(reply(String(url))); }));
});

afterEach(() => {
  logs.disposeLogs();
  closeSheet();
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("process log sheet session regressions", () => {
  it("keeps distinct roots and source/history nodes while an event boundary refreshes output", async () => {
    history = [{ ts: 1, app: "demo", action: "start", by: "panel", ok: true, message: "started" }];
    await logs.openLogs(app);
    expect(document.querySelectorAll("#prLogBlock")).toHaveLength(1);
    const capture = source("capture");
    const hist = node("prHistBlock");
    const historyRow = hist.querySelector("details") as HTMLDetailsElement;
    historyRow.open = true;
    reply = (url) => { return response(url.includes("/history?") ? { entries: history } : answer([header, "new run"], { size: 200 })); };
    beginFollow();
    await tick();
    expect(source("capture")).toBe(capture);
    expect(node("prHistBlock")).toBe(hist);
    expect(hist.querySelector("details")).toBe(historyRow);
    expect(historyRow.open).toBe(true);
    expect(node("prLogPre").textContent).toBe("new run");
  });

  it("an app-authored header-shaped line renders as one plain block and never forces the event repaint", async () => {
    reply = (url) => {
      if (url.includes("/history?")) return response({ entries: [] });
      if (url.includes("source=app")) return response(answer([header, "app line"], { source: "app", path: "app.log", size: 100 }));
      return response(answer(["capture line"]));
    };
    await logs.openLogs(app);
    source("app").click();
    await settle();
    // One plain block: the header-shaped app line is TEXT, not an event
    // boundary, so no disclosure is born from the app's own words.
    expect(node("prLogBlock").querySelectorAll("details")).toHaveLength(0);
    expect(node("prLogBlock").querySelectorAll(".lrow")).toHaveLength(0);
    const pre = node("prLogPre");
    expect(pre.textContent).toContain(header);
    expect(pre.textContent).toContain("app line");
    (pre as unknown as { __tag: number }).__tag = 3;
    // An appended header-shaped line stays data too: no new event, and the
    // repaint a new header triggers on the capture never fires - the pre
    // node the reader may be inside is patched, not replaced.
    reply = (url) => {
      if (url.includes("/history?")) return response({ entries: [] });
      if (url.includes("source=app")) return response(answer(["--- 2026-10-09T03:00:00Z stop by panel ---", "more"], { source: "app", path: "app.log", size: 200 }));
      return response(answer(["capture line"]));
    };
    beginFollow();
    await tick();
    expect(node("prLogBlock").querySelectorAll("details")).toHaveLength(0);
    expect(node("prLogPre")).toBe(pre);
    expect((node("prLogPre") as unknown as { __tag?: number }).__tag).toBe(3);
    expect(pre.textContent).toContain("stop by panel");
    expect(pre.textContent).toContain("more");
    expect(pre.textContent).toContain("app line");
  });

  it("reset with a headerless tail drops the old event DOM instead of appending to its newest pre", async () => {
    reply = (url) => { return response(url.includes("/history?") ? { entries: [] } : answer([header, "old output"])); };
    await logs.openLogs(app);
    expect(node("prLogBlock").querySelectorAll("details")).toHaveLength(1);
    const capture = source("capture");
    reply = (url) => { return response(url.includes("/history?") ? { entries: [] } : answer(["rotated output"], { reset: true, size: 14 })); };
    beginFollow();
    await tick();
    expect(node("prLogBlock").querySelectorAll("details")).toHaveLength(0);
    expect(node("prLogPre").textContent).toBe("rotated output");
    expect(node("prLogBlock").textContent).not.toContain("old output");
    expect(source("capture")).toBe(capture);
  });

  it("bounds rendered event output at 500 lines after headerless append trims older events", async () => {
    const old = Array.from({ length: 490 }, (_, i) => { return "old-" + i; });
    reply = (url) => { return response(url.includes("/history?") ? { entries: [] } : answer([header, ...old])); };
    await logs.openLogs(app);
    const more = Array.from({ length: 40 }, (_, i) => { return "new-" + i; });
    reply = (url) => { return response(url.includes("/history?") ? { entries: [] } : answer(more, { size: 200 })); };
    beginFollow();
    await tick();
    const visible = Array.from(node("prLogBlock").querySelectorAll("pre")).flatMap((pre) => { return (pre.textContent || "").split("\n"); });
    expect(visible).toHaveLength(500);
    expect(visible[0]).toBe("old-30");
    expect(visible.at(-1)).toBe("new-39");
    expect(node("prLogBlock").textContent).not.toContain("old-0\n");
  });

  it("serializes poll requests and queues the latest source without accepting an old source response", async () => {
    await logs.openLogs(app);
    const pending = deferred();
    reply = (url) => {
      if (url.includes("/history?")) return response({ entries: [] });
      return url.includes("source=app") ? response(answer(["app only"], { source: "app", path: "app.log", size: 7 })) : pending.promise;
    };
    beginFollow();
    await tick();
    await tick();
    await tick();
    expect(logReads()).toHaveLength(2);
    source("app").click();
    await settle();
    pending.resolve(response(answer(["stale capture"], { size: 999 })));
    await settle();
    expect(source("app").getAttribute("aria-selected")).toBe("true");
    expect(node("prLogPre").textContent).toBe("app only");
    expect(node("prLogPre").textContent).not.toContain("stale capture");
    expect(logReads().at(-1)).toContain("source=app&tail=500");
    reply = (url) => { return response(url.includes("/history?") ? { entries: [] } : answer([], { source: "app", size: 7 })); };
    await tick();
    expect(logReads().at(-1)).toContain("source=app&after=7");
  });

  it("does not repaint or toast an obsolete response after another sheet replaces the log root", async () => {
    await logs.openLogs(app);
    const pending = deferred();
    reply = () => { return pending.promise; };
    beginFollow();
    await tick();
    showSheet(sheet({ title: "Other", body: "other sheet", foot: [] }));
    pending.resolve(response({ error: "obsolete failure" }, 500));
    await settle();
    expect(node("sheet").textContent).toContain("other sheet");
    expect(node("prLogBlock")).toBeNull();
    expect(node("toast").hidden).toBe(true);
    const count = requests.length;
    await tick();
    expect(requests).toHaveLength(count);
  });

  it("ignores an earlier open's history response when a later app has already opened", async () => {
    const pending = deferred();
    reply = (url) => {
      if (url.includes("/history?app=demo")) return pending.promise;
      if (url.includes("/history?")) return response({ entries: [] });
      return response(answer([url.includes("/other/") ? "other output" : "stale output"]));
    };
    const first = logs.openLogs(app);
    await settle();
    await logs.openLogs({ ...app, name: "other" });
    pending.resolve(response({ entries: [{ ts: 1, app: "demo", action: "start", by: "panel", ok: false, message: "stale history" }] }));
    await first;
    await settle();
    expect(node("sheet").textContent).toContain("other logs");
    expect(node("prLogPre").textContent).toBe("other output");
    expect(node("sheet").textContent).not.toContain("stale history");
  });

  it("missing log 404 remains an inline state without repeated poll toasts and recovers on first start", async () => {
    reply = (url) => { return response(url.includes("/history?") ? { entries: [] } : { error: "missing capture" }, url.includes("/history?") ? 200 : 404); };
    await logs.openLogs(app);
    expect(node("prLogBlock").textContent).toContain("does not exist yet");
    expect(node("toast").hidden).toBe(true);
    beginFollow();
    await tick();
    await tick();
    expect(node("toast").hidden).toBe(true);
    reply = (url) => { return response(url.includes("/history?") ? { entries: [] } : answer([header, "first start"])); };
    await tick();
    expect(node("prLogPre").textContent).toBe("first start");
    expect(node("prLogBlock").textContent).not.toContain("does not exist yet");
  });

  it("pairs a stop after exit with that run before a later start, never with an older unfinished run", async () => {
    history = [
      { ts: 1, app: "demo", action: "start", by: "panel", ok: true, message: "old incomplete" },
      { ts: 1000, app: "demo", action: "start", by: "panel", ok: true, message: "recent start" },
      { ts: 2000, app: "demo", action: "exit", by: "plugin", ok: true, message: "recent exit", exitCode: 0 },
      { ts: 2100, app: "demo", action: "stop", by: "cli", ok: true, message: "recent stop" },
      { ts: 3000, app: "demo", action: "start", by: "panel", ok: true, message: "next start" },
    ];
    await logs.openLogs(app);
    const runs = node("prHistBlock").querySelectorAll(".lrow");
    expect(runs).toHaveLength(3);
    expect(runs[1].querySelector(".lrow-sub")?.textContent).toContain("stopped by cli");
    expect(runs[1].querySelector("pre")?.textContent).toContain("recent stop");
    expect(runs[0].textContent).not.toContain("recent stop");
    expect(runs[2].textContent).not.toContain("recent stop");
  });

  it("refreshes changed history at new output boundaries but preserves unchanged idle history", async () => {
    await logs.openLogs(app);
    const block = node("prHistBlock");
    const before = historyReads().length;
    history = [{ ts: 1, app: "demo", action: "start", by: "panel", ok: true, message: "newly started" }];
    reply = (url) => { return response(url.includes("/history?") ? { entries: history } : answer([header, "new run"], { size: 200 })); };
    beginFollow();
    await tick();
    expect(historyReads()).toHaveLength(before + 1);
    expect(node("prHistBlock")).toBe(block);
    expect(block.textContent).toContain("newly started");
    const run = block.querySelector("details");
    reply = (url) => { return response(url.includes("/history?") ? { entries: history } : answer([], { size: 200 })); };
    await tick();
    expect(historyReads()).toHaveLength(before + 1);
    expect(block.querySelector("details")).toBe(run);
    source("capture").click();
    await settle();
    expect(historyReads()).toHaveLength(before + 2);
  });

  it("exposes follow's pressed state and restores it when stopped", async () => {
    await logs.openLogs(app);
    const follow = node("prFollow");
    expect(follow.getAttribute("aria-pressed")).toBe("false");
    beginFollow();
    expect(follow.getAttribute("aria-pressed")).toBe("true");
    logs.stopFollow();
    expect(follow.getAttribute("aria-pressed")).toBe("false");
    expect(follow.textContent).toBe("Follow");
  });

  it("disposing during initial reads stops follow and cannot reopen the sheet when answers arrive", async () => {
    const pendingLog = deferred();
    const pendingHistory = deferred();
    reply = (url) => { return url.includes("/history?") ? pendingHistory.promise : pendingLog.promise; };
    const opening = logs.openLogs(app);
    expect(node("sheet").hidden).toBe(false);
    beginFollow();
    logs.disposeLogs();
    expect(node("sheet").hidden).toBe(true);
    pendingLog.resolve(response(answer(["late initial output"])));
    pendingHistory.resolve(response({ entries: [] }));
    await opening;
    await tick();
    expect(node("sheet").hidden).toBe(true);
    expect(node("sheet").textContent).toBe("");
    expect(requests).toHaveLength(2);
    expect(node("toast").hidden).toBe(true);
  });

  it("disposing a replaced log session leaves the replacement sheet open", async () => {
    await logs.openLogs(app);
    beginFollow();
    showSheet(sheet({ title: "Other", body: "replacement", foot: [] }));
    logs.disposeLogs();
    expect(node("sheet").hidden).toBe(false);
    expect(node("sheet").textContent).toContain("replacement");
    const before = requests.length;
    await tick();
    expect(requests).toHaveLength(before);
  });

  it("a reset-bearing initial tail retains its new lines rather than treating reset as an empty file", async () => {
    reply = (url) => { return response(url.includes("/history?") ? { entries: [] } : answer(["new tail"], { reset: true })); };
    await logs.openLogs(app);
    expect(node("prLogPre").textContent).toBe("new tail");
  });

  it("updates newest event count without replacing its pre or older disclosures", async () => {
    reply = (url) => { return response(url.includes("/history?") ? { entries: [] } : answer([header, "old run", "--- 2026-10-09T02:00:00Z start by cli ---", "fresh"])); };
    await logs.openLogs(app);
    const newest = node("prLogPre");
    const events = Array.from(node("prLogBlock").querySelectorAll("details"));
    events[0].open = true;
    reply = (url) => { return response(url.includes("/history?") ? { entries: [] } : answer(["tick"], { size: 200 })); };
    beginFollow();
    await tick();
    expect(node("prLogPre")).toBe(newest);
    expect(Array.from(node("prLogBlock").querySelectorAll("details"))).toEqual(events);
    expect(events[0].open).toBe(true);
    expect(newest.textContent).toBe("fresh\ntick");
    expect(events[1].closest(".lrow")?.querySelector("[data-prlogcount]")?.textContent).toBe("2 lines");
  });

  it("refreshes history on headerless new output and deduplicates repeated poll failures", async () => {
    history = [{ ts: 1, app: "demo", action: "start", by: "panel", ok: true, message: "started" }];
    await logs.openLogs(app);
    history.push({ ts: 1000, app: "demo", action: "exit", by: "plugin", ok: true, message: "finished", exitCode: 0 });
    reply = (url) => { return response(url.includes("/history?") ? { entries: history } : answer(["exited"], { size: 200 })); };
    beginFollow();
    await tick();
    expect(node("prHistBlock").textContent).toContain("exit 0");
    const hist = node("prHistBlock").querySelector("details");
    reply = (url) => { return response(url.includes("/history?") ? { entries: history } : { error: "read failed" }, url.includes("/history?") ? 200 : 500); };
    await tick();
    expect(node("toast").textContent).toBe("read failed");
    const output = node("prLogBlock").firstChild;
    await vi.advanceTimersByTimeAsync(5000);
    expect(node("toast").hidden).toBe(true);
    expect(node("prLogBlock").firstChild).toBe(output);
    expect(node("prHistBlock").querySelector("details")).toBe(hist);
  });
});
