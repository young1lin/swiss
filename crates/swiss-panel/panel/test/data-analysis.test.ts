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
const calls: string[] = [];
let seq = 0;
let task: AnalysisTask;
let execution: Promise<unknown> | null = null;
const previousFetch = globalThis.fetch;
globalThis.fetch = vi.fn(async (input: RequestInfo | URL) => {
  const url = String(input);
  calls.push(url);
  let out: unknown = {};
  if (url.endsWith("/execute")) out = await execution;
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
  execution = null;
  Object.assign(state(), { conn: "c", conns: [dbConn("c", "mysql")] });
  task = { id: "a" + (++seq), conn: "c", title: "Orders", current: "v1", saved: false,
    revisions: [{ id: "v1", parent: null, sql: "SELECT id, amount FROM orders", conditions: { grain: "order" }, note: "", at: "2026-11-01" }],
    executions: [], results: [{ id: "r1", revision: "v1", execution: "e1", rowCount: 51, truncated: false, verification: "undetermined", checks: [], note: "", superseded: false, chart: null }] };
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
  const rerun = (): HTMLButtonElement => [...document.querySelectorAll<HTMLButtonElement>("button")].find(b => b.textContent === "Rerun")!;
  await vi.waitFor(() => expect(rerun()).toBeTruthy());
  rerun().click();
  await vi.waitFor(() => expect(calls.some(url => url.endsWith("/execute"))).toBe(true));
  await analysis.dbAnalysisOpen(task.id);
  finish({ result: task.results[0] });
  await vi.waitFor(() => expect(rerun().disabled).toBe(false));
});
