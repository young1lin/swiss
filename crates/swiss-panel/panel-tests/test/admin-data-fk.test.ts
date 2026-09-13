import { describe, it, expect } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Same DOM-stub technique as admin-data-cellview.test.ts: the panel ships browser ES modules,
// so the module graph needs the globals stubbed before it will evaluate under Node.
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
  Blob: class {}, setInterval: () => 0, clearInterval: () => 0,
  addEventListener: () => {}, removeEventListener: () => {},
});

const admin = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets");
const view = await import(pathToFileURL(join(admin, "js", "data-view.js")).href) as {
  dbFkJump: (fk: unknown, value: unknown) => { schema: string | null; table: string; filters: unknown[] } | null;
  dbFocusedColumnValue: (d: unknown, column: string) => unknown;
};

/* docs/22 W5.2 — the FK jump. The payload is pure: describe_table's FK row plus the focused
   row's value become exactly the open-table state a typed filter would have built (the W1.5
   channel), or nothing at all when there is no value to equal. */
describe("dbFkJump — the payload one FK jump opens", () => {
  const fk = { name: "fk_orders_customer", column: "customer_id", refSchema: "public", refTable: "customers", refColumn: "id" };

  it("opens the referenced table with one eq filter on the referenced column", () => {
    expect(view.dbFkJump(fk, 42)).toEqual({
      schema: "public",
      table: "customers",
      filters: [{ column: "id", op: "eq", value: 42 }],
    });
  });

  it("flattens an empty refSchema to null — a schemaless dialect opens schemaless", () => {
    var j = view.dbFkJump({ ...fk, refSchema: "" }, "abc");
    expect(j!.schema).toBeNull();
    expect(j!.table).toBe("customers");
  });

  it("keeps the value's type — a bigint string stays an exact string", () => {
    var j = view.dbFkJump(fk, "9223372036854775807");
    expect((j!.filters[0] as { value: string }).value).toBe("9223372036854775807");
  });

  it("refuses NULL and missing values — col = NULL matches nothing, so there is no jump", () => {
    expect(view.dbFkJump(fk, null)).toBeNull();
    expect(view.dbFkJump(fk, undefined)).toBeNull();
    expect(view.dbFkJump(null, 1)).toBeNull();
  });
});

describe("dbFocusedColumnValue — the header arrow's row (docs/22 W5.2)", () => {
  const mk = (focus: unknown) => ({
    focus,
    inserts: [{ values: { customer_id: 7 } }],
    data: { rows: [{ id: 1, customer_id: 42 }, { id: 2, customer_id: null }] },
  });

  it("reads a data row past the buffered inserts", () => {
    expect(view.dbFocusedColumnValue(mk({ r: 1, c: 0 }), "customer_id")).toBe(42);
  });

  it("reads a buffered insert's own value", () => {
    expect(view.dbFocusedColumnValue(mk({ r: 0, c: 0 }), "customer_id")).toBe(7);
  });

  it("returns undefined (not null) when nothing is focused — null IS a value here", () => {
    expect(view.dbFocusedColumnValue(mk(null), "customer_id")).toBeUndefined();
    expect(view.dbFocusedColumnValue(null, "customer_id")).toBeUndefined();
  });

  it("returns null for a focused NULL cell — the caller says so, not the helper", () => {
    expect(view.dbFocusedColumnValue(mk({ r: 2, c: 0 }), "customer_id")).toBeNull();
  });
});

describe("the sprite carries the FK jump's arrow", () => {
  it("i-arrow-right exists — a straight arrow, not the pagination chevron", () => {
    const shell = readFileSync(join(admin, "index.html"), "utf8");
    expect(shell).toContain('<symbol id="i-arrow-right"');
  });
});
