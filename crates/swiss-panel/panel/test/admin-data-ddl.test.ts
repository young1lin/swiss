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

// Same DOM-stub technique as admin-data-stats.test.ts: the panel ships browser ES modules, so
// the module graph needs the globals stubbed before it will evaluate under Node.
const el = (): Record<string, unknown> => {
  const node: Record<string, unknown> = {
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
  };
  node.cloneNode = () => el();
  return node;
};
const doc = {
  documentElement: el(), body: el(), head: el(),
  hidden: false, visibilityState: "visible", activeElement: null,
  getElementById: () => el(), createElement: () => el(), createTextNode: () => el(),
  querySelector: () => null, querySelectorAll: () => [],
  addEventListener: () => {}, removeEventListener: () => {},
};
Object.assign(globalThis, {
  document: doc, window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {},
  Blob: class {}, setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const mod = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-ddl.ts")).href
) as {
  dbDdlTypeOptions: (d: string) => string[];
  dbDdlDiffColumns: (oldCols: unknown[], rows: unknown[]) => { added: unknown[]; changed: unknown[]; removed: unknown[] };
  dbDdlFormPayload: (kind: string, form: Record<string, unknown>) => Record<string, unknown> | null;
  dbDdlIndexSuggestion: (t: string, cols: string[]) => string;
};

// SPEC §data.ddl: the pure layer under the DDL sheets — the (op, payload) body both endpoints
// take, the three-bucket diff that keeps the commit to its added bucket, and the datalist's
// suggestions. The SQL itself is built server-side; these pin what the panel sends it.
describe("dbDdlTypeOptions", () => {
  it("offers the common core to both dialects", () => {
    expect(mod.dbDdlTypeOptions("mysql")).toContain("varchar(255)");
    expect(mod.dbDdlTypeOptions("pg")).toContain("varchar(255)");
    expect(mod.dbDdlTypeOptions("mysql")).toContain("timestamp");
  });

  it("adds the dialect's own spells", () => {
    expect(mod.dbDdlTypeOptions("pg")).toContain("timestamptz");
    expect(mod.dbDdlTypeOptions("pg")).toContain("jsonb");
    expect(mod.dbDdlTypeOptions("mysql")).toContain("int unsigned");
    expect(mod.dbDdlTypeOptions("pg")).not.toContain("int unsigned");
  });
});

describe("dbDdlDiffColumns", () => {
  const oldCols = [
    { name: "id", dataType: "int", nullable: false, defaultValue: null, comment: null },
    { name: "name", dataType: "varchar(40)", nullable: true, defaultValue: "''", comment: "标签" },
  ];

  it("sorts rows into the three buckets against the old state", () => {
    const d = mod.dbDdlDiffColumns(oldCols, [
      { name: "id", type: "int", nullable: false, default: "", comment: "" }, // unchanged
      { name: "email", type: "text", nullable: true, default: "", comment: "" }, // new
    ]);
    expect(d.added).toHaveLength(1);
    expect((d.added[0] as { name: string }).name).toBe("email");
    expect(d.changed).toHaveLength(0);
    expect(d.removed).toHaveLength(1);
    expect((d.removed[0] as { name: string }).name).toBe("name");
  });

  it("classifies a same-name row with different facts as changed, not added", () => {
    const d = mod.dbDdlDiffColumns(oldCols, [
      { name: "id", type: "bigint", nullable: false, default: "", comment: "" },
    ]);
    expect(d.changed).toHaveLength(1);
    expect(d.added).toHaveLength(0);
  });

  it("ignores rows without a name and survives empty inputs", () => {
    const d = mod.dbDdlDiffColumns(oldCols, [{ name: "  ", type: "int", nullable: true, default: "", comment: "" }]);
    expect(d.added).toHaveLength(0);
    expect(d.removed).toHaveLength(2);
    expect(mod.dbDdlDiffColumns([], []).added).toHaveLength(0);
  });
});

describe("dbDdlFormPayload", () => {
  it("builds the create_table payload with only filled optional fields", () => {
    const p = mod.dbDdlFormPayload("table", {
      schema: "public", table: "cfg", comment: "配置",
      columns: [
        { name: "id", type: "bigint", nullable: false, default: "0", comment: "" },
        { name: "label", type: "varchar(40)", nullable: true, default: "", comment: "标签" },
      ],
    });
    expect(p).toEqual({
      schema: "public", table: "cfg", comment: "配置",
      columns: [
        { name: "id", type: "bigint", nullable: false, default: "0" },
        { name: "label", type: "varchar(40)", nullable: true, comment: "标签" },
      ],
    });
  });

  it("stays null until the table has a name and a column", () => {
    expect(mod.dbDdlFormPayload("table", { table: "", columns: [{ name: "a", type: "int" }] })).toBeNull();
    expect(mod.dbDdlFormPayload("table", { table: "t", columns: [] })).toBeNull();
  });

  it("keeps half-filled rows in so the server's refusal can name them", () => {
    const p = mod.dbDdlFormPayload("table", { table: "t", columns: [{ name: "a", type: "" }] });
    expect(p!.columns).toEqual([{ name: "a", type: "", nullable: false }]);
  });

  it("commits only the added bucket on add_column", () => {
    const p = mod.dbDdlFormPayload("column", {
      table: "cfg",
      oldColumns: [{ name: "id", dataType: "int", nullable: false, defaultValue: null, comment: null }],
      columns: [
        { name: "id", type: "int", nullable: false, default: "", comment: "" }, // prefilled old row
        { name: "email", type: "text", nullable: true, default: "", comment: "联系" }, // new row
      ],
    });
    expect(p!.columns).toEqual([{ name: "email", type: "text", nullable: true, comment: "联系" }]);
    expect(p!.table).toBe("cfg");
  });

  it("carries schema when set and omits a false unique", () => {
    const p = mod.dbDdlFormPayload("index", {
      schema: "app", table: "users", index: "users_name_idx",
      indexColumns: ["name", "", "order"], unique: false,
    });
    expect(p).toEqual({ schema: "app", table: "users", index: "users_name_idx", columns: ["name", "order"] });
  });

  it("marks a unique index and refuses a nameless one", () => {
    const p = mod.dbDdlFormPayload("index", { table: "t", index: "t_a_idx", indexColumns: ["a"], unique: true });
    expect(p!.unique).toBe(true);
    expect(mod.dbDdlFormPayload("index", { table: "t", index: "", indexColumns: ["a"] })).toBeNull();
    expect(mod.dbDdlFormPayload("index", { table: "t", index: "i", indexColumns: [] })).toBeNull();
  });
});

describe("dbDdlIndexSuggestion", () => {
  it("spells table, columns, _idx in lower case", () => {
    expect(mod.dbDdlIndexSuggestion("Users", ["Name", "email"])).toBe("users_name_email_idx");
  });

  it("caps under the identifier whitelist's 64", () => {
    expect(mod.dbDdlIndexSuggestion("t", ["a_very_long_column_name_xxxxxxxxxxxxxxxx"]).length).toBeLessThanOrEqual(63);
  });

  it("drops the column run when nothing is picked yet", () => {
    expect(mod.dbDdlIndexSuggestion("users", [])).toBe("users_idx");
  });
});
