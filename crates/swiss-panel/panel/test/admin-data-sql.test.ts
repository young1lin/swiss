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

import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { dbConn, dbTab, mountDbView, unmountDbView } from "../src/db-state.js";

// The panel ships browser ES modules; under Node they evaluate only with DOM globals stubbed —
// the same technique (and stub surface) as the boot check in admin-panel.test.ts and the sort
// cycle in admin-data-grid.test.ts.
/* h.ts tells a props bag from a child with `instanceof Node` (h.ts:96, :129), so a stub
   element has to BE a Node: a plain literal makes h() read an element in the props slot as
   a props bag, and frag() text-ifies every child - a divergence no assertion here would
   catch until the browser ran it. */
class StubNode {}
const el = (): Record<string, unknown> => {
  const node: Record<string, unknown> = Object.assign(Object.create(StubNode.prototype) as Record<string, unknown>, {
    style: {}, dataset: {}, hidden: false, disabled: false, checked: false, value: "",
    textContent: "", innerHTML: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    setAttribute: () => {}, getAttribute: () => "", removeAttribute: () => {},
    addEventListener: () => {}, removeEventListener: () => {},
    appendChild: (c: unknown) => c, removeChild: (c: unknown) => c, remove: () => {},
    querySelector: () => null, querySelectorAll: () => [],
    focus: () => {}, blur: () => {}, click: () => {},
    getBoundingClientRect: () => ({ left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }),
    contains: () => false, closest: () => null, insertAdjacentHTML: () => {},
  });
  node.cloneNode = () => el();
  return node;
};
const doc = {
  documentElement: el(), body: el(), head: el(),
  hidden: false, visibilityState: "visible", activeElement: null,
  getElementById: (_id?: string) => el(), createElement: () => el(), createTextNode: () => el(),
  createDocumentFragment: () => el(),
  querySelector: () => null, querySelectorAll: () => [],
  addEventListener: () => {}, removeEventListener: () => {},
};
const prevWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
Object.assign(globalThis, {
  document: doc, window: globalThis, Node: StubNode,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {},
  Blob: class {}, setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

// Non-literal specifier: the module graph is browser JS outside tsc's purview, same as the
// admin-panel boot test's imports.
const sql = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-sql.ts")).href
) as {
  dbWithExplain: (s: string, mode?: string) => string;
  dbSplitStatements: (t: string) => string[];
  dbResultTabLabel: (stmt: string, rowCount?: number) => string;
  dbPendingSql: () => string[];
  dbCommit: () => Promise<void>;
  renderDbBar: () => void;
};

/* The pending-SQL preview and the Commit gate read the Data view's record, which db-state.ts
   owns since docs/37 R4. Plain static import, unlike data-sql.ts above: that one dodges tsc
   because its module graph is browser JS, while this is an ordinary typed module — and a
   literal specifier is also what keeps this binding the SAME instance data-sql.ts resolves.
   The unmount/mount pair is the reset, which is what assigning over the whole record did. */
const setTable = (over: Record<string, unknown> = {}) => {
  unmountDbView();
  mountDbView();
  Object.assign(dbConn(), {
    conn: "mysql",
    conns: [{ name: "mysql", dialect: "mysql" }],
    sqlOpen: false, sqlText: "", sqlResult: null, sqlResults: null, resultTab: 0, sqlBusy: false,
  });
  const t = dbTab();
  if (t.kind === "table") {
    Object.assign(t, {
      schema: "",
      table: "t",
      data: { primaryKey: ["id"], columns: [{ name: "id" }, { name: "a" }, { name: "b" }] },
      deletes: {},
      updates: {},
      inserts: [],
      ...over,
    });
  }
};

// docs/22 W0.5: the Explain button offers plain EXPLAIN and EXPLAIN ANALYZE; the prefix is
// built by one pure helper so this file can pin both spellings without a DOM.
describe("dbWithExplain modes", () => {
  it("prefixes a plain statement with EXPLAIN", () => {
    expect(sql.dbWithExplain("SELECT 1")).toBe("EXPLAIN SELECT 1");
    expect(sql.dbWithExplain("select 1")).toBe("EXPLAIN select 1");
  });

  it("mode analyze spells EXPLAIN ANALYZE", () => {
    expect(sql.dbWithExplain("SELECT 1", "analyze")).toBe("EXPLAIN ANALYZE SELECT 1");
    expect(sql.dbWithExplain("UPDATE t SET a = 1", "analyze")).toBe("EXPLAIN ANALYZE UPDATE t SET a = 1");
  });

  it("never stacks a second EXPLAIN, in either mode", () => {
    expect(sql.dbWithExplain("EXPLAIN SELECT 1")).toBe("EXPLAIN SELECT 1");
    expect(sql.dbWithExplain("EXPLAIN SELECT 1", "analyze")).toBe("EXPLAIN SELECT 1");
    expect(sql.dbWithExplain("EXPLAIN ANALYZE SELECT 1", "analyze")).toBe("EXPLAIN ANALYZE SELECT 1");
  });

  it("strips one trailing terminator before prefixing", () => {
    expect(sql.dbWithExplain("SELECT 1; ")).toBe("EXPLAIN SELECT 1");
    expect(sql.dbWithExplain("SELECT 1; ", "analyze")).toBe("EXPLAIN ANALYZE SELECT 1");
  });
});

// docs/22 W4.3: the console splits the caret's block on statement-level semicolons and runs
// each reply into its own result tab. The split is one pure helper because it must never cut
// inside a string literal or a comment — a naive split(";") shreds both.
describe("dbSplitStatements", () => {
  it("splits plain statements and drops empty pieces", () => {
    expect(sql.dbSplitStatements("SELECT 1; SELECT 2")).toEqual(["SELECT 1", "SELECT 2"]);
    expect(sql.dbSplitStatements("SELECT 1;; ;SELECT 2;")).toEqual(["SELECT 1", "SELECT 2"]);
    expect(sql.dbSplitStatements("  \n SELECT 1 ;\n\t")).toEqual(["SELECT 1"]);
    expect(sql.dbSplitStatements(";;;\n; ")).toEqual([]);
  });

  it("keeps inner newlines — the statement renders exactly as typed", () => {
    expect(sql.dbSplitStatements("SELECT 1\nFROM t;\nUPDATE t\nSET a = 2;")).toEqual([
      "SELECT 1\nFROM t",
      "UPDATE t\nSET a = 2",
    ]);
  });

  it("does not split inside single-quoted strings, including doubled quotes and escapes", () => {
    expect(sql.dbSplitStatements("SELECT 'a;b'; SELECT 'it''s'; SELECT 'x\\'; SELECT 1")).toEqual([
      "SELECT 'a;b'",
      "SELECT 'it''s'",
      "SELECT 'x\\'",
      "SELECT 1",
    ]);
  });

  it("does not split inside double-quoted identifiers or backtick names", () => {
    expect(sql.dbSplitStatements('SELECT "a;b"; SELECT `x;y`')).toEqual([
      'SELECT "a;b"',
      "SELECT `x;y`",
    ]);
  });

  it("does not split inside line or block comments", () => {
    expect(sql.dbSplitStatements("SELECT 1 -- note; not a cut\n; SELECT 2")).toEqual([
      "SELECT 1 -- note; not a cut",
      "SELECT 2",
    ]);
    expect(sql.dbSplitStatements("/* header; still one */ SELECT 1; SELECT 2 /* tail; */")).toEqual([
      "/* header; still one */ SELECT 1",
      "SELECT 2 /* tail; */",
    ]);
    expect(sql.dbSplitStatements("SELECT 1 # mysql note; no cut\n; SELECT 2")).toEqual([
      "SELECT 1 # mysql note; no cut",
      "SELECT 2",
    ]);
  });

  it("a whole-block comment-only stretch yields nothing to run", () => {
    expect(sql.dbSplitStatements("-- just a note; nothing else")).toEqual([]);
  });
});

// docs/22 W4.3: a tab's name is the statement's first word plus its row count — the shape the
// spec pins ("SELECT · 42"). EXPLAIN answers name themselves because the prefix ran too.
describe("dbResultTabLabel", () => {
  it("first word uppercased plus row count", () => {
    expect(sql.dbResultTabLabel("SELECT 1", 42)).toBe("SELECT · 42");
    expect(sql.dbResultTabLabel("select 1", 0)).toBe("SELECT · 0");
    expect(sql.dbResultTabLabel("EXPLAIN ANALYZE SELECT 1", 23)).toBe("EXPLAIN · 23");
  });

  it("no count renders the word alone; a lost word still names the tab", () => {
    expect(sql.dbResultTabLabel("UPDATE t SET a = 1", undefined)).toBe("UPDATE");
    expect(sql.dbResultTabLabel("\n  -- leading comment\n  SELECT 1", 3)).toBe("SELECT · 3");
    expect(sql.dbResultTabLabel("", 5)).toBe("? · 5");
  });
});

/* docs/42 T1 regression pin: the bar's gate reads the ACTIVE TAB's buffers, and the
   rewrite once dropped master's unhide — a buffered edit left the bar hidden and every
   re-render APPENDED another button set (no fill). The visibility half is what a stub can
   see; the append-reset half is the real-browser walkthrough's. */
describe("the edit bar's visibility (docs/42 T1)", () => {
  it("hides with nothing buffered, unhides the moment an edit is buffered", () => {
    const bar = el();
    doc.getElementById = (id?: string) => (id === "dbBar" ? bar : el());
    setTable();
    sql.renderDbBar();
    expect(bar.hidden, "nothing buffered hides the bar").toBe(true);
    setTable({ updates: { k1: { pk: { id: 1 }, changes: { a: "x" } } } });
    sql.renderDbBar();
    expect(bar.hidden, "a buffered update shows the bar").toBe(false);
    setTable();
    sql.renderDbBar();
    expect(bar.hidden, "dropping the buffer hides it again").toBe(true);
  });
});

// docs/22 W4.1 (W4b follow-up): the preview must sketch the address the SERVER will really
// use - pk columns when the table has one, every column when it has none, with the md5 fold
// the server applies to long text (EDIT_ADDR_MD5_MIN = 64) - instead of promising a primary
// key it does not have and printing an empty WHERE for exactly the keyless tables.
describe("dbPendingSql addressing", () => {
  it("a pk table's delete stays addressed by its primary key", () => {
    setTable({ deletes: { "7": { id: 7 } } });
    expect(sql.dbPendingSql()).toEqual(["DELETE FROM `t` WHERE `id` = 7;"]);
  });

  it("a keyless delete is addressed by every column, not an empty WHERE", () => {
    setTable({
      data: { primaryKey: [], columns: [{ name: "a" }, { name: "b" }] },
      deletes: { "1|x": { a: 1, b: "x" } },
    });
    expect(sql.dbPendingSql()).toEqual(["DELETE FROM `t` WHERE `a` = 1 AND `b` = 'x';"]);
  });

  it("a keyless update addresses by every column too", () => {
    setTable({
      data: { primaryKey: [], columns: [{ name: "a" }, { name: "b" }] },
      updates: { "1|x": { pk: { a: 1, b: "x" }, changes: { b: "y" } } },
    });
    expect(sql.dbPendingSql()).toEqual(["UPDATE `t` SET `b` = 'y' WHERE `a` = 1 AND `b` = 'x';"]);
  });

  it("long values and hex binaries sketch the server's md5 fold", () => {
    const bs = String.fromCharCode(92); // one backslash, kept out of the source literals
    const bin = bs + "xff0041"; // the wire form of a binary column: \x + contiguous hex
    const long = "n".repeat(70);
    setTable({
      data: { primaryKey: [], columns: [{ name: "note" }, { name: "payload" }] },
      deletes: { "1": { note: long, payload: bin } },
    });
    const out = sql.dbPendingSql()[0];
    expect(out).toContain("MD5(`note`) = md5('" + long + "')");
    expect(out).toContain("MD5(`payload`) = md5('" + bin + "')");
  });

  it("a NULL keyless column refuses the row with the server's own reasoning", () => {
    setTable({
      data: { primaryKey: [], columns: [{ name: "a" }, { name: "b" }] },
      deletes: { "1": { a: 1, b: null } },
    });
    expect(() => sql.dbPendingSql()).toThrow(/no primary key.*b is NULL/);
  });
});

// The Commit gate names the addressing too, because "every row is addressed by its primary
// key" was a lie for keyless tables - the user deserves to know the WHERE is whole-row.
describe("dbCommit confirm wording", () => {
  it("says primary key for a pk table", async () => {
    let asked = "";
    (globalThis as { confirm: (m: string) => boolean }).confirm = (m: string) => { asked = m; return false; };
    setTable({ deletes: { "7": { id: 7 } } });
    await sql.dbCommit();
    expect(asked).toContain("addressed by its primary key");
  });

  it("says every column for a keyless table", async () => {
    let asked = "";
    (globalThis as { confirm: (m: string) => boolean }).confirm = (m: string) => { asked = m; return false; };
    setTable({
      data: { primaryKey: [], columns: [{ name: "a" }, { name: "b" }] },
      deletes: { "1|x": { a: 1, b: "x" } },
    });
    await sql.dbCommit();
    expect(asked).toContain("no primary key");
    expect(asked).toContain("all its columns");
  });
});
// Leave the worker as we found it, as far as we can.
if (prevWindow) Object.defineProperty(globalThis, "window", prevWindow);
