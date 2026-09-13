import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Same DOM-stub technique as admin-data-grep.test.ts: the panel ships browser ES modules, so
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
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "data-sql.js")).href
) as { dbApplyReadback: (rows: Record<string, unknown>[], pk: string[], pkVals: Record<string, unknown>, row: Record<string, unknown>) => Record<string, unknown>[] };

// docs/22 W1.7: the commit reply's read-back rows are folded into the page by primary key, so
// the server's kept value (a truncated varchar, a DEFAULT, a trigger rewrite) replaces the
// typed one on screen.
describe("dbApplyReadback", () => {
  it("patches the matching row and leaves the rest alone", () => {
    const rows = [{ id: 1, name: "toolongvalue" }, { id: 2, name: "b" }];
    const out = mod.dbApplyReadback(rows, ["id"], { id: 1 }, { id: 1, name: "tru" });
    expect(out[0].name).toBe("tru");
    expect(out[1].name).toBe("b");
  });

  it("matches a composite key across both columns", () => {
    const rows = [{ a: 1, b: 1, v: "x" }, { a: 1, b: 2, v: "y" }];
    mod.dbApplyReadback(rows, ["a", "b"], { a: 1, b: 2 }, { a: 1, b: 2, v: "kept" });
    expect(rows[0].v).toBe("x");
    expect(rows[1].v).toBe("kept");
  });

  it("is a no-op without a key, without rows, or on a miss", () => {
    expect(mod.dbApplyReadback([{ id: 1 }], [], { id: 1 }, { id: 1 })).toEqual([{ id: 1 }]);
    expect(mod.dbApplyReadback(null as never, ["id"], { id: 1 }, { id: 1 })).toBe(null);
    const rows = [{ id: 1, v: "x" }];
    mod.dbApplyReadback(rows, ["id"], { id: 9 }, { id: 9, v: "z" });
    expect(rows[0].v).toBe("x");
  });
});
