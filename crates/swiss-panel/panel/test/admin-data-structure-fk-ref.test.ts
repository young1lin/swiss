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

/* The Structure tab's Foreign Keys list draws each target as a button that opens the
   referenced table (docs/22 W5.2). Since docs/37 R5 the button carries its target in data
   attributes and #pane's delegated click answers it via `closest("[data-ref-table]")` +
   `dataset.refTable`. The R5 conversion spelled the h() data keys camelCase
   (`{ refTable, refSchema }`); h() writes data keys verbatim, the HTML parser lowercases
   attribute names, and the button ended up carrying data-reftable - which neither the
   selector nor dataset.refTable ever matched. The button was inert on every FK row until
   the 2026-09-20 review. The DOM-stub suites could not see it (their setAttribute keeps the
   key's case and their closest() knows only #ids), so this runs on a real DOM. */

import { describe, it, expect, beforeAll, afterAll, beforeEach } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { dbConn as dbConnState, dbTab, mountDbView, unmountDbView } from "../src/db-state.js";
import { dbConn } from "./db-fixtures.js";
import type { ApiDbFkRow } from "../src/types/api.js";

const here = dirname(fileURLToPath(import.meta.url));

/* The served shell's ids, as the other happy-dom view suites build them: several modules in
 * the Data view's import graph wire a shell control at evaluation time. */
function shellSkeleton(): string {
  const html = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

let structure: {
  renderDbDetailGrid: (wrap: HTMLElement) => void;
  dbStructureClick: (t: Element, ev: MouseEvent) => boolean;
};

const fk: ApiDbFkRow = { name: "fk_orders_customer", column: "customer_id", refSchema: "", refTable: "customers", refColumn: "id" };

beforeAll(async () => {
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  // The click's dbOpenTable kicks off the opened table's data and detail loads; one empty
  // answer serves both shapes (a page with no rows, a detail with nothing in it).
  const empty = {
    rows: [], columns: [], primaryKey: [], total: 0, editable: false,
    schema: "", table: "customers", indexes: [], foreignKeys: [], ddl: "",
  };
  Object.assign(globalThis, {
    fetch: () => Promise.resolve({ status: 200, ok: true, json: () => Promise.resolve(empty) }),
    confirm: () => true,
  });
  afterAll(() => {
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
  });
  document.body.innerHTML = shellSkeleton();
  structure = await import("../src/data-structure.js");
});

beforeEach(() => {
  document.body.innerHTML = shellSkeleton();
  unmountDbView();
  mountDbView();
  Object.assign(dbConnState(), { conns: [dbConn("c", "mysql")], conn: "c" });
  const tab = dbTab();
  if (tab.kind === "table") {
    Object.assign(tab, {
      table: "orders", schema: null, pane: "fks",
      detail: { schema: "", table: "orders", columns: [], primaryKey: [], indexes: [], foreignKeys: [fk], ddl: "" },
    });
  }
});

describe("Structure > Foreign Keys > the referenced-table button (docs/22 W5.2, R5 delegation)", () => {
  it("carries its target where the dispatcher looks: [data-ref-table] / dataset.refTable", () => {
    const wrap = document.createElement("div");
    structure.renderDbDetailGrid(wrap);
    const btn = wrap.querySelector<HTMLElement>("button.db-fk-ref");
    expect(btn, "the FK target is a button").not.toBeNull();
    expect(btn!.textContent).toBe(".customers (id)");
    expect(btn!.matches("[data-ref-table]"), "the delegated selector matches it").toBe(true);
    expect(btn!.dataset.refTable).toBe("customers");
    expect(btn!.dataset.refSchema).toBe("");
  });

  it("a click on it opens the referenced table", () => {
    const wrap = document.createElement("div");
    document.getElementById("pane")!.appendChild(wrap);
    structure.renderDbDetailGrid(wrap);
    const btn = wrap.querySelector<HTMLElement>("button.db-fk-ref")!;
    const handled = structure.dbStructureClick(btn, new MouseEvent("click", { bubbles: true }));
    expect(handled).toBe(true);
    const d = dbTab();
    expect(d.kind, "the ref opens a table tab").toBe("table");
    if (d.kind !== "table") return;
    expect(d.table).toBe("customers");
    expect(d.schema).toBeNull(); // an empty refSchema opens schemaless (dbOpenTable's contract)
    expect(d.pane).toBe("data");
  });
});
