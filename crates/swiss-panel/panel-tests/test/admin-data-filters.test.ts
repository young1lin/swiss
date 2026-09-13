import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Same DOM-stub technique as admin-data-sql.test.ts: the panel ships browser ES modules, so
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

const filters = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "data-filters.js")).href
) as {
  DB_FILTER_OPS: { op: string; label: string }[];
  dbValueless: (op: string) => boolean;
};

// docs/22 W1.2: the operator dropdown carries in / notIn / between alongside the comparison
// ops, mirroring the server's BROWSE_FILTER_OPS whitelist. The three are value operators —
// picking one must keep the value input on screen (only isNull/isNotNull are valueless).
describe("DB_FILTER_OPS", () => {
  it("offers in, notIn and between", () => {
    const ops = filters.DB_FILTER_OPS.map((o) => o.op);
    expect(ops).toContain("in");
    expect(ops).toContain("notIn");
    expect(ops).toContain("between");
  });

  it("labels every op (the dropdown shows the label, never the op id)", () => {
    for (const o of filters.DB_FILTER_OPS) expect(o.label.length).toBeGreaterThan(0);
  });

  it("treats the list operators as value operators", () => {
    expect(filters.dbValueless("in")).toBe(false);
    expect(filters.dbValueless("notIn")).toBe(false);
    expect(filters.dbValueless("between")).toBe(false);
    expect(filters.dbValueless("isNull")).toBe(true);
    expect(filters.dbValueless("isNotNull")).toBe(true);
  });
});
