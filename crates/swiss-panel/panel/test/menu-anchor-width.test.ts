import { describe, it, expect, afterAll } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

/* popupMenu's anchor-width floor (found live on 19998): a menu dropped from a row must
   never sit narrower than the row that opened it. The connection menu measured 160px
   against its 233px sidebar row because the CSS content floor won. The floor is now
   max(CSS floor, anchor width), set as an inline min-width BEFORE the measuring rect
   and the clamp, so the clamp sees the true width. These cases pin that contract.

   The DOM is stubbed (this suite runs in the node environment): popupMenu needs so
   little of it that a full happy-dom would be the larger dependency. */

class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

const el = (tag: string): Record<string, any> => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, textContent: "", id: "", className: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Record<string, any>) {
      if (c.tag === "#document-fragment") { c.children.forEach((k: Record<string, any>) => { n.children.push(k); }); c.children = []; return c; }
      n.children.push(c); return c;
    },
    removeChild(c: Record<string, any>) { n.children = n.children.filter((x: Record<string, any>) => x !== c); return c; },
    remove() {}, contains: () => false, closest: () => null,
    setAttribute() {}, getAttribute: () => "", removeAttribute() {},
    addEventListener() {}, removeEventListener() {}, dispatchEvent: () => true,
    querySelector: () => null, querySelectorAll: () => [],
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 0, height: 0, right: 0, bottom: 0 }),
  };
  Object.setPrototypeOf(n, NodeStub.prototype);
  return n;
};

const body = el("body");
const byId: Record<string, Record<string, any>> = {};

const D = (globalThis as unknown as { document: unknown }).document;
Object.assign(globalThis, {
  document: {
    createElement: (t: string) => el(t),
    createElementNS: (_ns: string, t: string) => el(t),
    createDocumentFragment: () => el("#document-fragment"),
    createTextNode: (s: string) => { const n = el("#text"); n.textContent = s; return n; },
    getElementById: (id: string) => (byId[id] || (byId[id] = el("div"))),
    querySelectorAll: () => [],
    // connect.ts registers its document-level click delegation at module top level, so the
    // stub must answer addEventListener BEFORE menu.js (via pane.js to connect.js) imports.
    addEventListener: () => {}, removeEventListener: () => {},
    body,
  },
  window: { innerWidth: 1280, innerHeight: 800, addEventListener: () => {}, removeEventListener: () => {} },
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { hash: "" },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, removeEventListener: () => {} }),
});

const here = dirname(fileURLToPath(import.meta.url));
const menu = await import(pathToFileURL(join(here, "..", "src", "menu.js")).href) as {
  popupMenu: (anchor: { left: number; top: number; bottom: number; width?: number }, items: unknown[]) => void;
};

const menuNode = (): Record<string, any> => {
  // The stub's remove() is a no-op, so closeMenu leaves the previous menu on the body -
  // each popupMenu appends a fresh node, and the one under test is the LAST one.
  const menus = body.children.filter((c: Record<string, any>) => String(c.className).includes("menu float"));
  const n = menus[menus.length - 1];
  expect(n, "the menu was appended to the body").toBeTruthy();
  return n as Record<string, any>;
};

describe("popupMenu's anchor-width floor", () => {
  it("a row's width becomes the menu's floor - never narrower than what opened it", () => {
    menu.popupMenu({ left: 64, top: 90, bottom: 110, width: 233 }, [{ label: "mysql", fn: () => {} }]);
    // 233 > 160, so the anchor decides: the menu is at least as wide as the sidebar row.
    expect(menuNode().style.minWidth).toBe("max(160px, 233px)");
  });

  it("a small anchor keeps the CSS floor - the stylesheet still decides", () => {
    menu.popupMenu({ left: 900, top: 40, bottom: 62, width: 22 }, [{ label: "Refresh", fn: () => {} }]);
    expect(menuNode().style.minWidth).toBe("max(160px, 22px)");
  });

  it("an anchor with no width (the pointer-anchored ctx menus) is untouched", () => {
    menu.popupMenu({ left: 300, top: 200, bottom: 200 }, [{ label: "Copy", fn: () => {} }]);
    expect(menuNode().style.minWidth).toBeFalsy();
  });
});

afterAll(() => {
  Object.assign(globalThis, { document: D });
});
