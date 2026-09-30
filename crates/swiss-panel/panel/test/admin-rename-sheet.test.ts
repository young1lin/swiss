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

import { beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// @vitest-environment happy-dom

/* SPEC §panel.ui: the three renames share ONE surface — the general one-field sheet
   (openFieldSheet in ui/sheet.ts) — and no browser dialog remains. The suite runs on a
   real DOM (happy-dom) with the served shell's ids in place, then drives the real entry
   points: detail.js's renameMcp, data-edit.js's dbTableMenu (its Rename item), and the
   group sheet itself. What is pinned is the CONTRACT: the sheet opens visibly, Save
   submits exactly what was typed, an illegal value errors inline and sends nothing, and
   the group path behaves as before the generalization. */

function shellSkeleton(): string {
  const html = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

interface Req { url: string; method: string; body: string }
const calls: Req[] = [];

let detail: typeof import("../src/detail.js");
let addSheet: typeof import("../src/add-sheet.js");
let edit: typeof import("../src/data-edit.js");
let mcpState: typeof import("../src/mcp-state.js");
let dbState: typeof import("../src/db-state.js");

beforeAll(async () => {
  const g = globalThis as unknown as Record<string, unknown>;
  g.window = { addEventListener() {}, innerWidth: 1440, innerHeight: 900 };
  g.location = { origin: "http://127.0.0.1:19999" };
  // happy-dom's localStorage is a getter: vitest 5 writes a global assignment through to
  // the window, where it throws, so the stub goes in the documented way.
  vi.stubGlobal("localStorage", { getItem: () => null, setItem() {}, removeItem() {} });
  g.fetch = async (url: string, opts: { method?: string; body?: string } = {}) => {
    calls.push({ url: String(url), method: opts.method || "GET", body: opts.body || "" });
    return { ok: true, status: 200, json: async () => ({}) };
  };
  document.body.innerHTML = shellSkeleton();
  mcpState = await import("../src/mcp-state.js");
  dbState = await import("../src/db-state.js");
  addSheet = await import("../src/add-sheet.js");
  detail = await import("../src/detail.js");
  edit = await import("../src/data-edit.js");
});

/** Let every submit chain (await apiJson -> sheet close / inline error) run to its end. */
const settle = () => new Promise((r) => { setTimeout(r, 0); });

const sheet = () => document.getElementById("sheet") as HTMLElement;
const field = () => document.getElementById("g-name") as HTMLInputElement;
const errBox = () => document.getElementById("g-err") as HTMLElement;
const save = () => (document.getElementById("g-save") as HTMLButtonElement).click();

beforeEach(() => { calls.length = 0; });

describe("SPEC §panel.ui — one one-field sheet for every rename", () => {
  it("an MCP rename submits the sheet's value as the POST /rename payload", async () => {
    mcpState.resetMcpState();
    mcpState.setMcpGroups(["default"]);
    mcpState.setMcpRows([{ name: "old", lifecycle: "stopped", state: "unknown", type: "http", tag: "http", source: "managed", description: "", group: "default" }] as never);
    detail.renameMcp("old");
    // The house sheet contract: visible BEFORE the body is painted, and the field seeds
    // with the current name so a rename is an edit, not a retype.
    expect(sheet().hidden).toBe(false);
    expect(field().value).toBe("old");
    field().value = "new-name";
    save();
    await settle();
    const rename = calls.find((c) => c.url.includes("/api/mcps/old/rename"));
    expect(rename, "the rename request fired").toBeTruthy();
    expect(rename!.method).toBe("POST");
    expect(JSON.parse(rename!.body)).toEqual({ name: "new-name" });
    expect(sheet().hidden).toBe(true); // closed on success
  });

  it("an illegal table name errors INSIDE the sheet and sends nothing", async () => {
    const c = dbState.dbConn() as unknown as Record<string, unknown>;
    c.conns = [{ name: "c", dialect: "mysql", label: "", state: "ready", group: "default" }];
    c.conn = "c";
    const t = dbState.freshTab("table") as unknown as Record<string, unknown>;
    dbState.dbResetTabs([t as never], 0);
    t.table = "users";
    t.schema = null;
    t.pane = "data";
    t.data = {
      table: "users", columns: [{ name: "id", dataType: "int", nullable: false, isPrimaryKey: true, defaultValue: null, comment: null }],
      rows: [{ id: 1 }], total: 1, offset: 0, limit: 50, nextPage: false, primaryKey: ["id"], editable: true,
    };
    // The REAL menu, the way the toolbar opens it; its Rename item must open the sheet.
    edit.dbTableMenu(document.createElement("button"));
    const items = Array.from(document.querySelectorAll("#menu button")) as HTMLButtonElement[];
    const renameItem = items.find((b) => (b.textContent || "").includes("Rename"));
    expect(renameItem, "the Table menu carries the Rename item").toBeTruthy();
    renameItem!.click();
    expect(sheet().hidden).toBe(false);
    expect(field().value).toBe("users");
    field().value = "not a name!";
    save();
    await settle();
    expect(errBox().hidden).toBe(false);
    expect(errBox().textContent).toContain("Not a valid table name");
    expect(calls.some((x) => x.url.includes("/ddl"))).toBe(false); // nothing was sent
    expect(sheet().hidden).toBe(false); // the typed value stays on the sheet
  });

  it("the group rename path is unchanged: same sheet, submit gets the new name", async () => {
    const got: string[] = [];
    addSheet.openGroupSheet("prod", (name: string) => { got.push(name); return true; });
    expect(sheet().hidden).toBe(false);
    expect(field().value).toBe("prod");
    field().value = "learn";
    save();
    await settle();
    expect(got).toEqual(["learn"]);
    expect(sheet().hidden).toBe(true); // true closes, exactly as before the generalization
  });

  it("an empty value errors inline instead of submitting anything", async () => {
    const got: string[] = [];
    addSheet.openGroupSheet(null, (name: string) => { got.push(name); return true; });
    field().value = "   ";
    save();
    await settle();
    expect(errBox().hidden).toBe(false);
    expect(got).toEqual([]); // submit never ran
    expect(sheet().hidden).toBe(false);
  });

  it("no browser dialog call remains in the sources (the #16 acceptance grep)", () => {
    // prompt( may live only in the two historical doc comments (add-sheet.ts,
    // connect.ts); a CALL site would put this over the line. Comment-continued lines
    // start with comment syntax, so the call-shape test is line-based.
    const srcDir = join(dirname(fileURLToPath(import.meta.url)), "..", "src");
    const walk = (dir: string): string[] => {
      const out: string[] = [];
      for (const f of readdirSync(dir).sort()) {
        const p = join(dir, f);
        if (statSync(p).isDirectory()) out.push(...walk(p));
        else if (f.endsWith(".ts")) out.push(readFileSync(p, "utf8"));
      }
      return out;
    };
    const offenders: string[] = [];
    for (const src of walk(srcDir)) {
      for (const line of src.split("\n")) {
        if (/prompt[(]/.test(line) && !/^[ \t]*(\/|\*)/.test(line)) offenders.push(line.trim());
      }
    }
    expect(offenders, "prompt( outside comments: " + offenders.join(" | ")).toEqual([]);
  });
});
