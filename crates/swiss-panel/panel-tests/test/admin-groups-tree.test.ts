import { describe, expect, it, beforeAll, afterAll } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { emptyLineText } from "../../src/admin_assets/js/group-logic.js";

/* The group component DOM contract (docs/20 section 4.1, the tree model): the head is a
 * tree node - chevron, folder, name, count - with the low-frequency actions after it and
 * the grip last, so the leading columns keep one stable x for the CSS indent contract.
 * The suite runs the real mountGroup under a hand-rolled DOM whose elements keep their
 * children (the navigation suite idiom, extended by one tree walk), plus pure wording and
 * the CSS numbers the head markup and base.css must stay in agreement on. */

/* eslint-disable @typescript-eslint/no-explicit-any */
function fakeNode(tag: string): any {
  const classes = new Set<string>();
  const node: any = {
    tagName: tag.toUpperCase(),
    className: "",
    title: "",
    type: "",
    textContent: "",
    innerHTML: "",
    hidden: false,
    draggable: false,
    dataset: {},
    children: [],
    classList: {
      add: function () { for (let i = 0; i < arguments.length; i++) classes.add(arguments[i]); node.className = Array.from(classes).join(" "); },
      remove: function () { for (let i = 0; i < arguments.length; i++) classes.delete(arguments[i]); node.className = Array.from(classes).join(" "); },
      toggle: function (c: string, on?: boolean) { const want = on === undefined ? !classes.has(c) : on; if (want) classes.add(c); else classes.delete(c); node.className = Array.from(classes).join(" "); },
      contains: function (c: string) { return classes.has(c); },
    },
    setAttribute: function () {},
    getAttribute: function () { return null; },
    appendChild: function (c: any) { node.children.push(c); return c; },
    addEventListener: function () {},
    removeEventListener: function () {},
    querySelector: function () { return null; },
    querySelectorAll: function () { return []; },
    getBoundingClientRect: function () { return { left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }; },
  };
  return node;
}

let mountGroup: (cfg: any, g: any) => any;

beforeAll(async () => {
  const prevDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  Object.assign(globalThis, {
    document: {
      createElement: (tag: string) => fakeNode(tag),
      createTextNode: (text: string) => ({ textContent: text }),
      querySelector: () => null,
      querySelectorAll: () => [],
      getElementById: () => fakeNode("div"),
      addEventListener: () => {},
      removeEventListener: () => {},
      documentElement: fakeNode("html"),
      body: fakeNode("body"),
    },
  });
  afterAll(() => {
    if (prevDocument) Object.defineProperty(globalThis, "document", prevDocument);
    else delete (globalThis as Record<string, unknown>).document;
  });
  mountGroup = (await import("../../src/admin_assets/js/groups.js")).mountGroup;
});

/** A side-density cfg with the drag plumbing stubbed: enough for mountGroup to build, none
 *  of it fired. rows become inert button nodes the way sidebar.js builds them. */
function cfg(rows: any[], collapsed: Record<string, boolean> = {}) {
  return {
    scope: "mcps",
    density: "side",
    names: ["default", "learn"],
    collapsed,
    noun: "MCP",
    addTitle: (g: string) => "Add an MCP to " + g,
    onAdd: () => {},
    reload: () => {},
    render: () => {},
    afterDrag: () => {},
    drag: { get: () => null, set: () => {} },
    dragGroup: { get: () => null, set: () => {} },
    rowNode: (m: any) => {
      const b = fakeNode("button");
      b.dataset.name = m.name;
      return b;
    },
    rowId: (m: any) => m.name,
    rowsById: () => rows,
    groupOfRow: () => "default",
    onMoveRow: () => {},
    onAssign: () => {},
  };
}

describe("group head - tree node anatomy", () => {
  it("leads with chevron + folder + name + count; actions after; the grip is LAST", () => {
    const wrap = mountGroup(cfg([{ name: "redis" }, { name: "mysql" }]), { name: "default", rows: [{ name: "redis" }, { name: "mysql" }] });
    expect(wrap.className).toContain("grp grp--side");
    const head = wrap.children[0];
    expect(head.className).toBe("grp-head");
    const kinds = head.children.map((c: any) => c.className);
    // The order IS the contract: the leading columns must start at the head first child,
    // because base.css measures the chevron/folder/name x positions from them.
    expect(kinds).toEqual(["grp-toggle", "grp-add", "grp-more", "grp-grip"]);
    const toggle = head.children[0];
    const chev = toggle.children[0];
    const folder = toggle.children[1];
    const name = toggle.children[2];
    const count = toggle.children[3];
    expect(chev.className).toBe("grp-chev");
    expect(folder.className).toBe("grp-folder");
    expect(folder.innerHTML).toContain("#i-folder");
    expect(name.className).toBe("grp-name");
    expect(name.textContent).toBe("default");
    expect(count.textContent).toBe("2");
  });

  it("says the fold state with aria-expanded; a collapsed group keeps its count", () => {
    const open = mountGroup(cfg([]), { name: "default", rows: [] });
    expect(open.className).not.toContain("collapsed");
    const folded = mountGroup(cfg([], { default: true }), { name: "default", rows: [] });
    expect(folded.className).toContain("collapsed");
    expect(folded.children[0].children[0].children[3].textContent).toBe("0");
    // The attribute itself is set through the stubbed setAttribute; pin the source line so
    // the a11y contract cannot drift silently.
    const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../../src/admin_assets/js/groups.js"), "utf8");
    expect(src).toContain('setAttribute("aria-expanded", String(!folded))');
  });

  it("an empty group keeps its place: one short quiet line", () => {
    const wrap = mountGroup(cfg([]), { name: "default", rows: [] });
    const body = wrap.children[1];
    expect(body.className).toBe("grp-body");
    expect(body.children[0].className).toBe("grp-empty");
    expect(body.children[0].textContent).toBe(emptyLineText(true));
  });

  it("members land in the body; page density wraps them in one card", () => {
    const side = mountGroup(cfg([{ name: "redis" }]), { name: "default", rows: [{ name: "redis" }] });
    expect(side.children[1].children[0].dataset.name).toBe("redis");
    const pageCfg: any = cfg([]);
    pageCfg.density = "page";
    pageCfg.rowNode = undefined;
    pageCfg.rowsHtml = () => "<div class='row'></div>";
    pageCfg.rowSel = () => ".row";
    pageCfg.wireRow = () => {};
    const page = mountGroup(pageCfg, { name: "default", rows: [{ name: "redis" }] });
    expect(page.className).toContain("grp--page");
    expect(page.children[1].children[0].className).toBe("group");
  });
});

describe("empty-line wording (pure)", () => {
  it("stays two words plus at most the drop affordance", () => {
    expect(emptyLineText(true)).toBe("No items — drop here or press +");
    expect(emptyLineText(false)).toBe("No items");
  });
});

describe("the tree CSS contract (over the shipped sheet)", () => {
  const base = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "styles", "base.css"), "utf8");

  it("the head is a node, not a band: no --sep-soft ground on .grp-head", () => {
    expect(base).toMatch(/\.grp-head \{[^}]*background: none/);
    expect(base).not.toMatch(/\.grp-head \{[^}]*--sep-soft/);
  });

  it("the guide drops through the chevron column and a folded group draws none", () => {
    expect(base).toMatch(/\.grp::before \{[^}]*border-left: 1px solid var\(--sep\)/);
    expect(base).toMatch(/\.grp\.collapsed::before \{ display: none; \}/);
  });

  it("the member gutter is a full tree step of body indent in both densities", () => {
    // The numbers behind the label-offset contract in base.css groups section (child text
    // sits 26/28px right of the parent label): changing them is a deliberate hierarchy
    // change, not a tweak.
    expect(base).toMatch(/\.grp-body \{ padding-left: 42px; \}/);
    expect(base).toMatch(/\.grp--page \.grp-body \{ padding-left: 54px; \}/);
  });
});
