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

import { beforeAll, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// @vitest-environment happy-dom

/* fix-plan #14: the V2 glyph cleanup's RENDERED side. admin-icons.test.ts pins the sprite
   and the source-level ban; this suite drives the real builders on a real DOM (happy-dom,
   served shell ids in place) and asks what the USER sees: the control buttons and labels
   carry the svg sprite references and none of the retired unicode glyphs — while the
   filter-operator dropdown KEEPS its typographic symbols (that is copy, not icon). */

function shellSkeleton(): string {
  const html = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  // The Data view builds its pane at RUNTIME, so its element ids are not in the shell -
  // the handful this suite's renders write into are stubbed beside it.
  ids.push("dbGridWrap", "dbFilters", "dbStatus", "dbConsole", "dbTables", "dbTabStrip", "c-keypath");
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

let filters: typeof import("../src/data-filters.js");
let grid: typeof import("../src/data-grid.js");
let run: typeof import("../src/run.js");
let sheets: typeof import("../src/tunnel-sheets.js");
let i18n: typeof import("../src/i18n.js");
let dbState: typeof import("../src/db-state.js");

interface Req { url: string }
const calls: Req[] = [];
const lsBacking = new Map<string, string>();

beforeAll(async () => {
  const g = globalThis as unknown as Record<string, unknown>;
  g.window = { addEventListener() {}, innerWidth: 1440, innerHeight: 900 };
  g.location = { origin: "http://127.0.0.1:19999" };
  g.localStorage = {
    getItem: (k: string) => (lsBacking.has(k) ? lsBacking.get(k)! : null),
    setItem: (k: string, v: string) => void lsBacking.set(k, v),
    removeItem: (k: string) => void lsBacking.delete(k),
    clear: () => lsBacking.clear(),
  };
  g.fetch = async (url: string) => {
    calls.push({ url: String(url) });
    if (String(url).includes("/api/tunnels/browse")) {
      return { ok: true, status: 200, json: async () => ({ dir: "/home/dev/app", parent: "/home", entries: [
        { path: "/home/dev/app/.ssh", name: ".ssh", dir: true },
        { path: "/home/dev/app/id_rsa", name: "id_rsa", dir: false },
      ] }) };
    }
    return { ok: true, status: 200, json: async () => ({}) };
  };
  document.body.innerHTML = shellSkeleton();
  dbState = await import("../src/db-state.js");
  i18n = await import("../src/i18n.js");
  filters = await import("../src/data-filters.js");
  grid = await import("../src/data-grid.js");
  run = await import("../src/run.js");
  sheets = await import("../src/tunnel-sheets.js");
  await i18n.loadLocale(); // English, the stubbed store has no preference
});

const htmlOf = (id: string) => (document.getElementById(id) as HTMLElement).innerHTML;

/** A table tab with data, the way the loaders leave it after a successful browse. */
function seedTable(): Record<string, unknown> {
  const c = dbState.dbConn() as unknown as Record<string, unknown>;
  c.conns = [{ name: "c", dialect: "mysql", label: "", state: "ready", group: "default" }];
  c.conn = "c";
  const t = dbState.freshTab("table") as unknown as Record<string, unknown>;
  dbState.dbResetTabs([t as never], 0);
  t.table = "users";
  t.schema = null;
  t.pane = "data";
  t.data = {
    table: "users",
    columns: [
      { name: "id", dataType: "int", nullable: false, isPrimaryKey: true, defaultValue: null, comment: null },
      { name: "note", dataType: "text", nullable: true, isPrimaryKey: false, defaultValue: null, comment: null },
    ],
    rows: [{ id: 1, note: "one" }],
    total: 1, offset: 0, limit: 50, nextPage: false, primaryKey: ["id"], editable: true,
  };
  return t;
}

describe("fix-plan #14 — the glyph sites render sprite icons, not unicode glyphs", () => {
  it("the grid header's PK marker is the key sprite, not a glyph", () => {
    seedTable();
    grid.renderDbGrid();
    const wrap = htmlOf("dbGridWrap");
    expect(wrap).toContain('href="#i-key"');
    expect(wrap).not.toContain("\u26bf");
    const keyMark = (document.getElementById("dbGridWrap") as HTMLElement).querySelector(".db-key") as HTMLElement;
    expect(keyMark, "the marker span exists").toBeTruthy();
    expect(keyMark!.title).toBe("primary key"); // the hover keeps the words
  });

  it("the row controls are the x and undo sprites — and both recomute with the delete state", () => {
    const t = seedTable();
    grid.renderDbGrid();
    let wrap = htmlOf("dbGridWrap");
    expect(wrap).toContain('data-rdel="0"');
    expect(wrap).toContain('href="#i-x"');
    expect(wrap).not.toContain("\u2715");
    // Buffer a delete the way the click handler would (the key is the pk tuple's JSON);
    // the same button flips to undo.
    t.deletes = { "[1]": true };
    grid.renderDbGrid();
    wrap = htmlOf("dbGridWrap");
    expect(wrap).toContain('href="#i-undo"');
    expect(wrap).not.toContain("\u21a9");
    expect(wrap).not.toContain("\u2715");
  });

  it("the Form tab's record action is the same x and undo sprites (docs/46 P7: it still drew the glyphs)", () => {
    const t = seedTable();
    t.pane = "form";
    grid.renderDbGrid();
    let wrap = htmlOf("dbGridWrap");
    expect(wrap).toContain('data-fact=""');
    expect(wrap).toContain('href="#i-x"');
    expect(wrap).not.toContain("✕");
    t.deletes = { "[1]": true };
    grid.renderDbGrid();
    wrap = htmlOf("dbGridWrap");
    expect(wrap).toContain('href="#i-undo"');
    expect(wrap).not.toContain("↩");
  });

  it("the filter row's remove button is the x sprite; the operator dropdown KEEPS its symbols", () => {
    const t = seedTable();
    t.filters = [{ column: "id", op: "eq", value: "1" }];
    filters.renderDbFilters();
    const box = htmlOf("dbFilters");
    expect(box).toContain('href="#i-x"');
    expect(box).not.toContain("\u2715");
    // The counter-case the plan pins: typographic operators are copy, not icons.
    expect(box).toContain("\u2260"); // does not equal
    expect(box).toContain("\u2265"); // at least
  });

  it("the run tab's history control wears the history sprite beside the words", () => {
    const kd = { items: [{ name: "echo", description: "", inputSchema: null }], nextCursor: undefined, total: 1, loaded: true, loading: false, error: null, cursors: [""], pageSize: 50 };
    const d = { name: "m", tab: "run", tools: kd, run: { tool: "echo", result: null, running: false, hist: [{ seq: 1, args: "x=1" }], histTool: "echo", histLoading: false, histOpen: false, histSelSeq: null, histFull: {}, histQ: "" } };
    const host = document.createElement("div");
    host.append(...[run.runBodyNode(d as never, { name: "m", lifecycle: "started", state: "unknown", type: "http", tag: "http", source: "managed", description: "", group: "default" } as never)].flat().filter((n): n is Node => n != null));
    const btn = host.querySelector("#r-hist") as HTMLElement;
    expect(btn, "the history control exists").toBeTruthy();
    expect(btn.innerHTML).toContain('href="#i-history"');
    expect(btn.textContent).toContain("Past runs (1)"); // the count still reads
    expect(btn.textContent).not.toContain("\u21ba");
  });

  it("the key picker's rows are folder and file sprites, Up is the arrow sprite", async () => {
    calls.length = 0;
    await sheets.openKeyPicker();
    await new Promise((r) => { setTimeout(r, 0); }); // openKeyPicker fires render() un-awaited
    const list = document.querySelector(".keylist") as HTMLElement;
    expect(list, "the picker rendered").toBeTruthy();
    const dir = list.querySelector("[data-dir]") as HTMLElement;
    const file = list.querySelector("[data-file]") as HTMLElement;
    expect(dir.innerHTML).toContain('href="#i-folder"');
    expect(dir.textContent).toContain(".ssh");
    expect(file.innerHTML).toContain('href="#i-file"');
    expect(file.textContent).toContain("id_rsa");
    const up = document.getElementById("b-up") as HTMLElement;
    expect(up.innerHTML).toContain('href="#i-arrow-up"');
    expect(up.textContent).toContain("Up");
    const whole = (document.querySelector(".backdrop") as HTMLElement).innerHTML;
    for (const banned of ["\ud83d\udcc1", "\ud83d\udcc4", "\u2191"]) expect(whole, banned).not.toContain(banned);
    // Close the picker so later cases start clean.
    (document.querySelector("[data-close]") as HTMLElement).click();
  });

  it("the swapped locale copies carry words only — in both languages", async () => {
    await i18n.loadLocale();
    for (const s of [i18n.tr("dataStream.pendingNew.other", { n: 3 }), i18n.tr("dataStream.pendingNewOver", { n: 300 }),
      i18n.tr("dataStructure.table"), i18n.tr("runHistory.pastRuns"), i18n.tr("terminal.nNewDown", { n: 2 }), i18n.tr("tunnelSheets.text")]) {
      for (const banned of ["\u2191", "\u2193", "\u25be", "\u21ba"]) expect(s, s + " carries a direction glyph").not.toContain(banned);
    }
    i18n.setLang("zh-CN");
    await i18n.loadLocale();
    for (const s of [i18n.tr("dataStream.pendingNew.other", { n: 3 }), i18n.tr("dataStructure.table"),
      i18n.tr("runHistory.pastRuns"), i18n.tr("terminal.nNewDown", { n: 2 }), i18n.tr("tunnelSheets.text")]) {
      for (const banned of ["\u2191", "\u2193", "\u25be", "\u21ba"]) expect(s, s + " carries a direction glyph").not.toContain(banned);
    }
    expect(i18n.tr("dataStructure.table")).toBe("\u8868"); // the word survived the glyph swap
    i18n.setLang("en");
    await i18n.loadLocale();
  });
});
