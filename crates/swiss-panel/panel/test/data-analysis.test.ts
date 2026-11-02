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
let seq = 0;
let task: AnalysisTask;
let execution: Promise<unknown> | null = null;
let saving: Promise<unknown> | null = null;
let cancellation: Promise<Response> | null = null;
let taskReads: Promise<Response>[] = [];
const previousFetch = globalThis.fetch;
globalThis.fetch = vi.fn(async (input: RequestInfo | URL) => {
  const url = String(input);
  calls.push(url);
  if (url.endsWith("/cancel") && cancellation) return cancellation;
  if (url === "/api/analysis/" + task.id && taskReads.length) return taskReads.shift()!;
  let out: unknown = {};
  if (url.endsWith("/execute")) out = await execution;
  else if (url.endsWith("/save") && saving) out = await saving;
  else if (url === "/api/analysis/" + task.id) out = task;
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
  shell(); localStorage.clear(); calls.length = 0; unmountDbView(); mountDbView();
  execution = null; saving = null;
  cancellation = null;
  taskReads = [];
  Object.assign(state(), { conn: "c", conns: [dbConn("c", "mysql")] });
  task = { id: "a" + (++seq), conn: "c", title: "Orders", current: "v1", saved: false,
    revisions: [{ id: "v1", parent: null, sql: "SELECT id, amount FROM orders", conditions: { grain: "order" }, note: "", at: "2026-11-01" }],
    executions: [], results: [{ id: "r1", revision: "v1", execution: "e1", rowCount: 51, truncated: false, verification: "undetermined", checks: [], note: "", superseded: false, chart: null }] };
});

it("disables Stop waiting through a repaint and restores it after a refusal", async () => {
  let finish!: (value: Response) => void;
  cancellation = new Promise(resolve => { finish = resolve; });
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1" }];
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
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1" }];
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
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1" }];
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
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1" }];
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
  running.executions = [{ id: "e1", status: "running", error: null, revision: "v1" }];
  firstRead(new Response(JSON.stringify(running), { status: 200 }));
  await vi.waitFor(() => expect(document.querySelector<HTMLButtonElement>('[data-action="stop"]')!.disabled).toBe(false));
  secondRead(new Response(JSON.stringify(running), { status: 200 }));
  finishExecution({});
});

it("restores the action when the server does not accept cancellation", async () => {
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1" }];
  await analysis.dbAnalysisOpen(task.id);
  cancellation = Promise.resolve(new Response(JSON.stringify({ cancelled: false }), { status: 200 }));
  const stop = (): HTMLButtonElement => document.querySelector<HTMLButtonElement>('[data-action="stop"]')!;
  stop().click();
  await vi.waitFor(() => expect(stop().disabled).toBe(false));
  expect(stop().getAttribute("aria-label")).toBe("Stop waiting");
});

it("does not let an older opening response undo the accepted stop", async () => {
  task.executions = [{ id: "e1", status: "running", error: null, revision: "v1" }];
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
