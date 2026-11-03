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

/* SPEC §data.grid: a grid that cannot edit still looks. The table cells wired their right-click
 * only inside the editable branch, so a query-only connection (every module's own SQLite under
 * §data.sqlite) and a row buffered for delete had NO cell menu at all: no copy, no View value,
 * no way to read a folded JSON cell or the bytes behind "(binary, N bytes)". The owner's report
 * (2026-11-03): right-click on swiss-memory did nothing. Run on happy-dom so the menu is met
 * the way a person meets it: a real contextmenu event, the #menu in the document, a click. */

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dbConn as dbConnState, dbTab, mountDbView, unmountDbView } from "../src/db-state.js";
import { tr } from "../src/i18n.js";
import { dbDecodeMessage, dbDecodedFacts, dbVectorText } from "../src/data-value.js";
import type { ApiDbDecoded } from "../src/types/api.js";
import { dbCol, dbConn, dbPage } from "./db-fixtures.js";

const here = dirname(fileURLToPath(import.meta.url));
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

let grid: { renderDbGrid: () => void };

beforeAll(async () => {
  document.body.innerHTML = shellSkeleton();
  grid = await import("../src/data-grid.js");
});

const JSON_TEXT = JSON.stringify({ kind: "pitfall", tags: ["sqlite", "panel"], body: "x".repeat(120) });

/** A query-only page the way sqlite_browser.rs answers: editable false, a BLOB as the \x wire. */
function mountReadOnlyGrid(over: { editable?: boolean; deleted?: boolean; dialect?: string } = {}): HTMLElement {
  document.body.innerHTML = shellSkeleton();
  document.getElementById("menu")?.remove();
  const wrap = document.body.appendChild(Object.assign(document.createElement("div"), { id: "dbGridWrap" }));
  unmountDbView();
  mountDbView();
  Object.assign(dbConnState(), {
    conns: [dbConn("swiss-memory", over.dialect || "sqlite")], conn: "swiss-memory", gridCfg: { widths: {}, hidden: [] },
  });
  const tab = dbTab();
  if (tab.kind !== "table") throw new Error("fresh state must hold a table tab");
  Object.assign(tab, {
    table: "notes", schema: "main", pane: "data", loading: false,
    focus: null, conflict: null, detail: null,
    updates: {}, deletes: over.deleted ? { "[1]": true } : {}, inserts: [], sel: {},
    data: dbPage({
      schema: "main", table: "notes",
      columns: [dbCol("id", { dataType: "INTEGER", isPrimaryKey: true }), dbCol("meta"), dbCol("vec", { dataType: "BLOB" }), dbCol("gone")],
      rows: [{ id: 1, meta: JSON_TEXT, vec: "\\x89504e470d0a1a0a", gone: null }],
      total: 1, primaryKey: ["id"], editable: !!over.editable,
    }),
  });
  grid.renderDbGrid();
  return wrap;
}

/** The data cell under a column, by its grid address (row 0: no buffered inserts here). */
const cellAt = (wrap: HTMLElement, c: number): HTMLElement =>
  wrap.querySelector<HTMLElement>('td[data-r="0"][data-c="' + c + '"]')!;
function rightClick(cell: HTMLElement): HTMLElement | null {
  cell.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true, clientX: 10, clientY: 10 }));
  return document.getElementById("menu");
}
const labels = (menu: HTMLElement): string[] => Array.from(menu.querySelectorAll("button")).map((b) => b.textContent ?? "");
const pick = (menu: HTMLElement, label: string): HTMLElement =>
  Array.from(menu.querySelectorAll<HTMLElement>("button")).find((b) => b.textContent === label)!;

/** The decode route as a test serves it: each request's URL is kept, the next reply is the
 *  queued one (by default "nothing decodes this cell"). */
let asked: string[] = [];
let replies: { status?: number; body: unknown }[] = [];
function stubDecode(): void {
  asked = [];
  replies = [];
  vi.stubGlobal("fetch", vi.fn(async (url: unknown) => {
    asked.push(String(url));
    const next = replies.shift() || { status: 200, body: { decoded: false } };
    const status = next.status || 200;
    return { ok: status < 400, status, json: async () => next.body };
  }));
}
/** Let the sheet's fetch and its .then land. */
const settle = async (): Promise<void> => { for (let i = 0; i < 4; i++) await new Promise((r) => setTimeout(r, 0)); };
const sheetBody = (): HTMLElement => document.querySelector<HTMLElement>("#sheet .sheet-body")!;

describe("a read-only table's cell menu (SPEC §data.grid, §data.sqlite)", () => {
  beforeEach(() => { document.getElementById("menu")?.remove(); stubDecode(); });
  afterEach(() => { vi.unstubAllGlobals(); });

  it("a query-only grid's cell opens the menu: copy and View value, no Edit in dialog", () => {
    const menu = rightClick(cellAt(mountReadOnlyGrid(), 1));
    expect(menu, "the menu opened on a read-only cell").not.toBeNull();
    const ls = labels(menu!);
    expect(ls).toContain(tr("logs.copyValue"));
    expect(ls).toContain(tr("dataCsv.viewValue"));
    expect(ls).toContain(tr("dataCsv.copyRowAsJson"));
    expect(ls, "a grid that cannot edit offers no editor").not.toContain(tr("dataCsv.editInDialog"));
  });

  it("View value on a JSON cell opens the read-only sheet with the whole document", () => {
    const menu = rightClick(cellAt(mountReadOnlyGrid(), 1));
    pick(menu!, tr("dataCsv.viewValue")).click();
    const sheet = document.getElementById("sheet")!;
    expect(sheet.hidden, "the viewer sheet opened").toBe(false);
    expect(sheet.querySelector(".db-val-json"), "JSON reads as the code block").not.toBeNull();
    expect(sheet.querySelector(".sheet-body")!.textContent).toContain("pitfall");
  });

  it("View value on a BLOB cell nothing decodes shows its bytes as a hex dump", async () => {
    const menu = rightClick(cellAt(mountReadOnlyGrid(), 2));
    pick(menu!, tr("dataCsv.viewValue")).click();
    await settle();
    expect(sheetBody().textContent).toContain("89 50 4e 47 0d 0a 1a 0a");
  });

  it("a double-click on a read-only cell opens the same viewer; a NULL cell has nothing to open", async () => {
    const wrap = mountReadOnlyGrid();
    cellAt(wrap, 2).dispatchEvent(new MouseEvent("dblclick", { bubbles: true, cancelable: true }));
    expect(document.getElementById("sheet")!.hidden, "double-click opened the viewer").toBe(false);
    await settle();
    expect(sheetBody().textContent).toContain("89 50 4e 47");
    expect(cellAt(wrap, 3).ondblclick, "NULL carries no viewer").toBeNull();
    const menu = rightClick(cellAt(wrap, 3));
    expect(labels(menu!), "NULL still copies").toContain(tr("logs.copyValue"));
    expect(labels(menu!)).not.toContain(tr("dataCsv.viewValue"));
  });

  it("a row buffered for delete on an editable grid keeps the menu too, minus the editor", () => {
    const menu = rightClick(cellAt(mountReadOnlyGrid({ editable: true, deleted: true }), 1));
    expect(menu, "the deleted row's cell still opens a menu").not.toBeNull();
    expect(labels(menu!)).toContain(tr("dataCsv.viewValue"));
    expect(labels(menu!)).not.toContain(tr("dataCsv.editInDialog"));
  });

  it("an editable row still offers Edit in dialog (the writable path is unchanged)", () => {
    const menu = rightClick(cellAt(mountReadOnlyGrid({ editable: true }), 1));
    expect(labels(menu!)).toContain(tr("dataCsv.editInDialog"));
  });
});

/* SPEC §data.sqlite "Decoded cells": the owner's report (2026-11-03) wanted to see what the
 * binary really is, not a hex dump. A module's own SQLite answers its binary cells through the
 * module that wrote them: the panel names the row by its primary key (never the bytes), and the
 * module reads the row again, opens and decompresses it, and answers text, JSON or a vector. */
describe("a module's binary cell reads as what it is (SPEC §data.sqlite)", () => {
  beforeEach(() => { document.getElementById("menu")?.remove(); stubDecode(); });
  afterEach(() => { vi.unstubAllGlobals(); });

  async function viewBlob(over: { dialect?: string } = {}): Promise<void> {
    const menu = rightClick(cellAt(mountReadOnlyGrid(over), 2));
    pick(menu!, tr("dataCsv.viewValue")).click();
    await settle();
  }

  it("asks the decode route by table, column and the row's primary key, never the bytes", async () => {
    await viewBlob();
    expect(asked).toHaveLength(1);
    const u = new URL(asked[0], "http://x");
    expect(u.pathname).toBe("/api/db/swiss-memory/decode");
    expect(u.searchParams.get("table")).toBe("notes");
    expect(u.searchParams.get("column")).toBe("vec");
    expect(JSON.parse(u.searchParams.get("key")!)).toEqual({ id: 1 });
    expect(asked[0], "the cell's bytes never leave the page").not.toContain("89504e47");
  });

  it("a decoded JSON body shows as the code block under its facts line; Raw hex toggles", async () => {
    replies.push({ body: {
      decoded: true, kind: "json", text: JSON.stringify({ model: "glm", messages: [{ role: "user" }] }),
      record: "call", recordId: "7",
      chunk: { part: 0, parts: 1, codec: "lz4", sealed: true, plain: 52, stored: 40, ofs: 0 },
    } });
    await viewBlob();
    const facts = document.getElementById("dbValFacts")!;
    expect(facts.textContent).toContain(tr("dataDecode.recordCall") + " 7");
    expect(facts.textContent).toContain(tr("dataDecode.part", { n: 1, of: 1 }));
    expect(facts.textContent).toContain(tr("dataDecode.sealed"));
    expect(sheetBody().querySelector(".db-val-json"), "JSON reads as the code block").not.toBeNull();
    expect(sheetBody().textContent).toContain("messages");
    const toggle = document.getElementById("dbValRawHex")!;
    toggle.click();
    expect(sheetBody().textContent, "Raw hex shows the stored bytes").toContain("89 50 4e 47");
    expect(toggle.textContent).toBe(tr("dataDecode.decodedView"));
    toggle.click();
    expect(sheetBody().querySelector(".db-val-json"), "and back").not.toBeNull();
  });

  it("a part cut from a longer body is text, with its place in the body on the facts line", async () => {
    replies.push({ body: {
      decoded: true, kind: "text", text: "ld\"}]} and the rest", record: "session", recordId: "s-1",
      chunk: { part: 1, parts: 2, codec: "raw", sealed: false, plain: 19, stored: 19, ofs: 65536 },
    } });
    await viewBlob();
    const facts = document.getElementById("dbValFacts")!.textContent!;
    expect(facts).toContain(tr("dataDecode.part", { n: 2, of: 2 }));
    expect(facts).toContain(tr("dataDecode.plainBlock"));
    expect(facts).toContain(tr("dataDecode.raw"));
    expect(sheetBody().querySelector(".db-val-pre")!.textContent).toBe("ld\"}]} and the rest");
  });

  it("a memory vector shows its numbers, width, norm and model", async () => {
    replies.push({ body: {
      decoded: true, kind: "vector", vector: [0.6, -0.8, 0, 0], record: "memory", recordId: "m-1",
      vectorFacts: { model: "embedding-3", dims: 4, norm: 1 },
    } });
    await viewBlob();
    const facts = document.getElementById("dbValFacts")!.textContent!;
    expect(facts).toContain(tr("dataDecode.recordMemory") + " m-1");
    expect(facts).toContain(tr("dataDecode.dims", { n: 4 }));
    expect(facts).toContain("embedding-3");
    expect(sheetBody().querySelector(".db-val-pre")!.textContent).toBe("0   0.600000 -0.800000  0.000000  0.000000");
  });

  it("a refusal is said above the hex dump, in the panel's words", async () => {
    replies.push({ status: 400, body: { error: "the sealed part did not open: no key on this machine opens it, or it is bound to another row" } });
    await viewBlob();
    const warn = document.getElementById("dbValDecodeErr");
    expect(warn, "the refusal shows").not.toBeNull();
    expect(warn!.textContent).toContain(tr("dataDecode.sealRefused"));
    expect(sheetBody().textContent, "the bytes stay readable").toContain("89 50 4e 47");
  });

  it("a user database's BLOB keeps the plain hex dump and asks nothing", async () => {
    await viewBlob({ dialect: "mysql" });
    expect(asked).toHaveLength(0);
    expect(document.getElementById("dbValRawHex"), "no decoded view to toggle").toBeNull();
    expect(sheetBody().textContent).toContain("89 50 4e 47");
  });

  it("the facts line, vector rows and refusal copy are pure", () => {
    const facts = dbDecodedFacts({
      decoded: true, record: "run", recordId: "42",
      chunk: { part: 0, parts: 3, codec: "lz4", sealed: true, plain: 65536, stored: 1200, ofs: 0 },
    } as ApiDbDecoded);
    expect(facts.split(" · ")).toHaveLength(5);
    expect(facts.startsWith(tr("dataDecode.recordRun") + " 42")).toBe(true);
    expect(dbDecodedFacts({ decoded: true, record: "later", recordId: "x" }), "an unknown noun shows as served").toBe("later x");
    const rows = dbVectorText(Array.from({ length: 10 }, (_, i) => i / 10)).split("\n");
    expect(rows).toHaveLength(2);
    expect(rows[1].startsWith("8 ")).toBe(true);
    expect(dbDecodeMessage("no chunk row has that key any more (pruned since the page was read)")).toBe(tr("dataDecode.rowGone"));
    expect(dbDecodeMessage("the memory service is not running")).toBe(tr("dataDecode.notRunning"));
    expect(dbDecodeMessage("engine said no")).toBe("engine said no");
  });
});
