/* Copyright 2026 young1lin. Licensed under the Apache License, Version 2.0. */
// @vitest-environment happy-dom
import { beforeEach, afterAll, it, expect, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { dbConn as state, mountDbView, unmountDbView, dbTab } from "../src/db-state.js";
import { dbConn } from "./db-fixtures.js";
import type { AnalysisTask } from "../src/types/analysis.js";

function shell(): void {
  const html = readFileSync(join(import.meta.dirname, "../../src/admin_assets/index.html"), "utf8");
  const ids = [...html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)].map(m => m[1]);
  document.body.innerHTML = [...new Set([...ids, "dbTabStrip", "dbHead", "dbMore", "dbFilters", "dbConsole", "dbSql", "dbSqlHl", "dbGridWrap", "dbBar", "dbRbar", "dbStatus", "dbTables"])].map(id => `<div id="${id}"></div>`).join("");
}
shell();
const { dbOpenTabForce } = await import("../src/data-tabs.js");
const calls: string[] = [];
const posts: { url: string; body: unknown }[] = [];
let seq = 0;
let task: AnalysisTask;
let execution: Promise<unknown> | null = null;
let saving: Promise<unknown> | null = null;
let cancellation: Promise<Response> | null = null;
let taskReads: Promise<Response>[] = [];
let seriesReply: unknown = null;
const previousFetch = globalThis.fetch;
globalThis.fetch = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
  const url = String(input);
  calls.push(url);
  if (init?.body) posts.push({ url, body: JSON.parse(String(init.body)) });
  if (url.endsWith("/cancel") && cancellation) return cancellation;
  if (url === "/api/analysis/" + task.id && taskReads.length) return taskReads.shift()!;
  let out: unknown = {};
  if (url.endsWith("/execute")) out = await execution;
  else if (url.endsWith("/revisions")) out = task;
  else if (url.endsWith("/save") && saving) out = await saving;
  else if (url === "/api/analysis/" + task.id) out = task;
  else if (url.includes("/series?")) out = seriesReply ?? { points: [{ x: "1", values: { amount: "10" } }, { x: "2", values: { amount: "20" } }, { x: "100", values: { amount: "30" } }], approximate: false, truncated: false };
  else if (url.includes("/rows?")) {
    const offset = Number(new URL(url, "http://localhost").searchParams.get("offset"));
    const n = offset === 0 ? 50 : 1;
    out = { result: "r1", columns: ["id", "amount"], rows: Array.from({ length: n }, (_, i) => ({ id: String(offset + i + 1), amount: "9007199254740993.01" })),
      rowIds: Array.from({ length: n }, (_, i) => `r1:${offset + i}`), offset, returned: n, total: 51, more: offset === 0, truncated: false };
  }
  return new Response(JSON.stringify(out), { status: 200, headers: { "Content-Type": "application/json" } });
});
const analysis = await import("../src/data-analysis.js");
afterAll(() => { globalThis.fetch = previousFetch; });
beforeEach(() => {
  shell(); localStorage.clear(); calls.length = 0; posts.length = 0; unmountDbView(); mountDbView();
  execution = null; saving = null;
  cancellation = null;
  taskReads = []; seriesReply = null;
  Object.assign(state(), { conn: "c", conns: [dbConn("c", "mysql")] });
  task = { id: "a" + (++seq), conn: "c", title: "Orders", current: "v1", saved: false, kind: "sql", createdAt: "2026-11-01T08:00:00Z", updatedAt: "2026-11-01T08:01:00Z",
    revisions: [{ id: "v1", parent: null, sql: "SELECT id, amount FROM orders", conditions: { grain: "order" }, note: "", params: {}, at: "2026-11-01T08:00:00Z" }],
    executions: [], results: [{ id: "r1", revision: "v1", execution: "e1", at: "2026-11-01T08:01:00Z", elapsedMs: 12, columns: ["id", "amount"], rowCount: 51, truncated: false, verification: "undetermined", checks: [], note: "", superseded: false, chart: null }] };
});

it("disables Stop waiting through a repaint and restores it after a refusal", async () => {
  let finish!: (value: Response) => void;
  cancellation = new Promise(resolve => { finish = resolve; });
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1", sql: "SELECT 1", params: {}, startedAt: "2026-11-01T08:00:00Z", finishedAt: null, elapsedMs: 0, rows: 0, truncated: false, result: null, superseded: false }];
  await analysis.dbAnalysisOpen(task.id);
  const stop = (): HTMLButtonElement => document.querySelector<HTMLButtonElement>('[data-action="stop"]')!;
  await vi.waitFor(() => expect(stop()).toBeTruthy());
  stop().focus();
  stop().click();
  expect(stop().disabled).toBe(true);
  expect(stop().getAttribute("aria-label")).toBe("Stopping wait…");
  stop().blur(); // A real browser removes focus when a focused button becomes disabled.
  await analysis.dbAnalysisOpen(task.id);
  expect(stop().disabled).toBe(true);
  stop().click();
  expect(calls.filter(url => url.endsWith("/cancel"))).toHaveLength(1);
  finish(new Response(JSON.stringify({ error: "refused" }), { status: 409 }));
  await vi.waitFor(() => expect(stop().disabled).toBe(false));
  expect(stop().getAttribute("aria-label")).toBe("Stop waiting");
  expect(stop().querySelector("use")?.getAttribute("href")).toBe("#i-stop");
  expect(document.activeElement).toBe(stop());
});

it("refreshes the execution after accepting Stop waiting", async () => {
  let finish!: (value: Response) => void;
  cancellation = new Promise(resolve => { finish = resolve; });
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1", sql: "SELECT 1", params: {}, startedAt: "2026-11-01T08:00:00Z", finishedAt: null, elapsedMs: 0, rows: 0, truncated: false, result: null, superseded: false }];
  await analysis.dbAnalysisOpen(task.id);
  const stop = (): HTMLButtonElement => document.querySelector<HTMLButtonElement>('[data-action="stop"]')!;
  stop().click();
  task.executions[0].status = "cancelled";
  finish(new Response(JSON.stringify({ execution: "e1", cancelled: true, databaseCancellationConfirmed: false }), { status: 200 }));
  await vi.waitFor(() => expect(stop().getAttribute("aria-label")).toBe("Stop waiting"));
  expect(stop().disabled).toBe(true);
  expect(calls.filter(url => url.endsWith("/cancel"))).toHaveLength(1);
});

it("keeps an accepted stop disabled when the status refresh fails", async () => {
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1", sql: "SELECT 1", params: {}, startedAt: "2026-11-01T08:00:00Z", finishedAt: null, elapsedMs: 0, rows: 0, truncated: false, result: null, superseded: false }];
  await analysis.dbAnalysisOpen(task.id);
  taskReads.push(Promise.resolve(new Response("unavailable", { status: 503 })));
  cancellation = Promise.resolve(new Response(JSON.stringify({ cancelled: true }), { status: 200 }));
  const stop = (): HTMLButtonElement => document.querySelector<HTMLButtonElement>('[data-action="stop"]')!;
  stop().click();
  await vi.waitFor(() => expect(calls.filter(url => url === "/api/analysis/" + task.id)).toHaveLength(2));
  await new Promise(resolve => setTimeout(resolve, 10));
  expect(stop().disabled).toBe(true);
  stop().click();
  expect(calls.filter(url => url.endsWith("/cancel"))).toHaveLength(1);
});

it("retries the status of an accepted stop started in another session", async () => {
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1", sql: "SELECT 1", params: {}, startedAt: "2026-11-01T08:00:00Z", finishedAt: null, elapsedMs: 0, rows: 0, truncated: false, result: null, superseded: false }];
  await analysis.dbAnalysisOpen(task.id);
  taskReads.push(Promise.resolve(new Response("unavailable", { status: 503 })));
  cancellation = Promise.resolve(new Response(JSON.stringify({ cancelled: true }), { status: 200 }));
  const stop = (): HTMLButtonElement => document.querySelector<HTMLButtonElement>('[data-action="stop"]')!;
  stop().click();
  await vi.waitFor(() => expect(calls.filter(url => url === "/api/analysis/" + task.id)).toHaveLength(2));
  task.executions[0].status = "cancelled";
  await vi.waitFor(() => expect(stop().dataset.analysisrun).toBe(""), { timeout: 4000 });
  expect(stop().disabled).toBe(true);
});

it("still activates an analysis when a newer status read overtakes opening", async () => {
  await analysis.dbAnalysisOpen(task.id);
  let finishRead!: (value: Response) => void;
  taskReads.push(new Promise(resolve => { finishRead = resolve; }));
  const reopening = analysis.dbAnalysisOpen(task.id);
  saving = Promise.resolve({});
  document.querySelector<HTMLButtonElement>('[data-action="bookmark"]')!.click();
  await vi.waitFor(() => expect(calls.filter(url => url === "/api/analysis/" + task.id)).toHaveLength(3));
  dbOpenTabForce({ kind: "sql" });
  expect((dbTab() as { analysisId?: string }).analysisId).not.toBe(task.id);
  finishRead(new Response(JSON.stringify(task), { status: 200 }));
  await reopening;
  expect((dbTab() as { analysisId?: string }).analysisId).toBe(task.id);
});

it("applies a slow polling response while a newer poll is still pending", async () => {
  await analysis.dbAnalysisOpen(task.id);
  let finishExecution!: (value: unknown) => void;
  execution = new Promise(resolve => { finishExecution = resolve; });
  let firstRead!: (value: Response) => void;
  let secondRead!: (value: Response) => void;
  taskReads.push(new Promise(resolve => { firstRead = resolve; }), new Promise(resolve => { secondRead = resolve; }));
  document.querySelector<HTMLButtonElement>('[data-action="start"]')!.click();
  await vi.waitFor(() => expect(calls.filter(url => url === "/api/analysis/" + task.id)).toHaveLength(3), { timeout: 3500 });
  const running = structuredClone(task);
  running.executions = [{ id: "e1", status: "running", error: null, revision: "v1", sql: "SELECT 1", params: {}, startedAt: "2026-11-01T08:00:00Z", finishedAt: null, elapsedMs: 0, rows: 0, truncated: false, result: null, superseded: false }];
  firstRead(new Response(JSON.stringify(running), { status: 200 }));
  await vi.waitFor(() => expect(document.querySelector<HTMLButtonElement>('[data-action="stop"]')!.disabled).toBe(false));
  secondRead(new Response(JSON.stringify(running), { status: 200 }));
  finishExecution({});
});

it("restores the action when the server does not accept cancellation", async () => {
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1", sql: "SELECT 1", params: {}, startedAt: "2026-11-01T08:00:00Z", finishedAt: null, elapsedMs: 0, rows: 0, truncated: false, result: null, superseded: false }];
  await analysis.dbAnalysisOpen(task.id);
  cancellation = Promise.resolve(new Response(JSON.stringify({ cancelled: false }), { status: 200 }));
  const stop = (): HTMLButtonElement => document.querySelector<HTMLButtonElement>('[data-action="stop"]')!;
  stop().click();
  await vi.waitFor(() => expect(stop().disabled).toBe(false));
  expect(stop().getAttribute("aria-label")).toBe("Stop waiting");
});

it("does not let an older opening response undo the accepted stop", async () => {
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1", sql: "SELECT 1", params: {}, startedAt: "2026-11-01T08:00:00Z", finishedAt: null, elapsedMs: 0, rows: 0, truncated: false, result: null, superseded: false }];
  await analysis.dbAnalysisOpen(task.id);
  let finishRead!: (value: Response) => void;
  taskReads.push(new Promise(resolve => { finishRead = resolve; }));
  const reopening = analysis.dbAnalysisOpen(task.id);
  cancellation = Promise.resolve(new Response(JSON.stringify({ cancelled: true }), { status: 200 }));
  const stop = (): HTMLButtonElement => document.querySelector<HTMLButtonElement>('[data-action="stop"]')!;
  stop().click();
  task.executions[0].status = "cancelled";
  await vi.waitFor(() => expect(stop().dataset.analysisrun).toBe(""));
  const stale = structuredClone(task); stale.executions[0].status = "running";
  finishRead(new Response(JSON.stringify(stale), { status: 200 }));
  await reopening;
  expect(stop().disabled).toBe(true);
  expect(stop().dataset.analysisrun).toBe("");
});

it("opens a server-owned analysis in the Data grid and carries stable selected row references", async () => {
  // SPEC §data.analysis: exercise the real grid/tab integration, not a standalone preview.
  await analysis.dbAnalysisOpen(task.id);
  await vi.waitFor(() => expect(document.querySelectorAll(".result-table tbody tr")).toHaveLength(50));
  expect(dbTab()).toMatchObject({ kind: "sql", analysisId: task.id });
  expect(document.getElementById("dbConsole")!.hidden).toBe(true);
  expect(document.querySelector(".result-table")!.textContent).toContain("9007199254740993.01");
  const pick = document.querySelector<HTMLInputElement>(".result-table input")!;
  pick.checked = true; pick.dispatchEvent(new Event("change"));
  expect(analysis.dbAnalysisContext()).toEqual({ taskId: task.id, resultId: "r1", rowIds: ["r1:0"] });
});

it("paginates the same immutable result instead of rerunning SQL", async () => {
  await analysis.dbAnalysisOpen(task.id);
  await vi.waitFor(() => expect(document.querySelector(".result-table")).not.toBeNull());
  const next = [...document.querySelectorAll<HTMLButtonElement>("button")].find(b => b.textContent === "Next")!;
  next.click();
  await vi.waitFor(() => expect(document.querySelectorAll(".result-table tbody tr")).toHaveLength(1));
  expect(calls).toContain(`/api/analysis/${task.id}/results/r1/rows?offset=50&limit=50`);
  expect(calls.some(url => url.includes("/execute"))).toBe(false);
});

it("reopening an in-flight analysis keeps the completion callback attached to its view", async () => {
  let finish!: (value: unknown) => void;
  execution = new Promise(resolve => { finish = resolve; });
  await analysis.dbAnalysisOpen(task.id);
  const rerun = (): HTMLButtonElement => [...document.querySelectorAll<HTMLButtonElement>("button")].find(b => b.getAttribute("aria-label") === "Rerun")!;
  await vi.waitFor(() => expect(rerun()).toBeTruthy());
  rerun().click();
  await vi.waitFor(() => expect(calls.some(url => url.endsWith("/execute"))).toBe(true));
  await analysis.dbAnalysisOpen(task.id);
  finish({ result: task.results[0] });
  await vi.waitFor(() => expect(rerun().disabled).toBe(false));
});


it("saving keeps the action disabled through a repaint and reverses its icon after success", async () => {
  let finish!: (value: unknown) => void;
  saving = new Promise(resolve => { finish = resolve; });
  await analysis.dbAnalysisOpen(task.id);
  const action = (): HTMLButtonElement => document.querySelector<HTMLButtonElement>('[data-action="bookmark"], [data-action="unbookmark"]')!;
  expect(action().querySelector("use")?.getAttribute("href")).toBe("#i-bookmark");
  action().click();
  await vi.waitFor(() => expect(action().disabled).toBe(true));
  expect(action().getAttribute("aria-label")).toBe("Saving analysis…");
  await analysis.dbAnalysisOpen(task.id);
  expect(action().disabled).toBe(true);
  task.saved = true; finish({ ok: true });
  await vi.waitFor(() => expect(action().getAttribute("aria-label")).toBe("Unsave analysis"));
  expect(action().querySelector("use")?.getAttribute("href")).toBe("#i-bookmark-minus");
});


it("opens the immutable result named by an Agent reference and preserves it on reopen", async () => {
  const old = { ...task.results[0], id: "r-old", at: "2026-10-31T08:00:00Z" };
  task.results.unshift(old);
  await analysis.dbAnalysisOpen(task.id, "c", old.id);
  await vi.waitFor(() => expect(calls.some(url => url.includes("/results/r-old/rows?"))).toBe(true));
  expect(analysis.dbAnalysisContext()?.resultId).toBe(old.id);
  await analysis.dbAnalysisOpen(task.id);
  expect(analysis.dbAnalysisContext()?.resultId).toBe(old.id);
  await analysis.dbAnalysisOpen(task.id, "c", "other-task-result");
  expect(analysis.dbAnalysisContext()?.resultId).toBe(old.id);
});

it("requests scatter observations rather than grouped sums and passes numeric x to the chart", async () => {
  task.results[0].chart = { kind: "scatter", x: "id", y: ["amount"], title: "Orders" };
  await analysis.dbAnalysisOpen(task.id);
  await vi.waitFor(() => expect(calls.some(url => url.includes("/series?") && url.includes("mode=scatter"))).toBe(true));
  await vi.waitFor(() => expect(document.querySelectorAll(".result-chart circle")).toHaveLength(3));
  const xs = [...document.querySelectorAll(".result-chart circle")].map(c => Number(c.getAttribute("cx")));
  expect((xs[2] - xs[1]) / (xs[1] - xs[0])).toBeCloseTo(98);
});

it("does not plot a zero for a category with no numeric values", async () => {
  seriesReply = { points: [{ x: "invalid", values: { amount: { sum: 0, n: 0 } } }, { x: "valid", values: { amount: { sum: 4, n: 1 } } }], invalid: 1, approximate: false, truncated: false };
  task.results[0].chart = { kind: "bar", x: "id", y: ["amount"], title: "Orders" };
  await analysis.dbAnalysisOpen(task.id);
  await vi.waitFor(() => expect(document.querySelectorAll(".result-chart rect")).toHaveLength(1));
  expect(document.querySelector(".result-chart rect title")?.textContent).toContain("valid: 4");
});

it("keeps condition structure and params when the reader revises SQL", async () => {
  task.revisions[0].conditions = { window: { from: "2026-10-01", to: "2026-11-01" }, excludeCancelled: true };
  await analysis.dbAnalysisOpen(task.id);
  document.querySelector<HTMLButtonElement>('[data-action="edit"]')!.click();
  const editors = [...document.querySelectorAll<HTMLTextAreaElement>('#sheet textarea')];
  const conditions = editors.find(e => e.value.includes("excludeCancelled"))!;
  expect(JSON.parse(conditions.value)).toEqual(task.revisions[0].conditions);
  const sql = editors.find(e => e !== conditions)!; sql.value += " WHERE id > 10";
  [...document.querySelectorAll<HTMLButtonElement>('#sheet button')].find(b => b.textContent === "Create revision and run")!.click();
  await vi.waitFor(() => expect(posts.find(p => p.url.endsWith("/revisions"))).toBeTruthy());
  expect(posts.find(p => p.url.endsWith("/revisions"))?.body).toMatchObject({ conditions: task.revisions[0].conditions, params: {} });
});


it("a durable chat artifact opens its captured historical result even when newer results exist", async () => {
  const old = { ...task.results[0], id: "r-artifact", at: "2026-10-31T08:00:00Z" };
  task.results.unshift(old);
  const reference = { taskId: task.id, resultId: old.id, title: "Orders", version: 1, run: 1, at: old.at };
  for (const text of [JSON.stringify({ reference, taskId: task.id, value: {} }), '{"reference":' + JSON.stringify(reference) + ',"taskId":"clipped']) {
    const artifact = analysis.dbAnalysisReference(text, "c");
    expect(artifact).toBeTruthy();
    document.body.appendChild(artifact!);
    const open = artifact!.querySelector<HTMLButtonElement>('button')!;
    expect(open.textContent).toContain("Version 1");
    open.click();
    await vi.waitFor(() => expect(analysis.dbAnalysisContext()?.resultId).toBe(old.id));
  }
  expect(analysis.dbAnalysisReference('{"reference":null}', "c")).toBeNull();
});
