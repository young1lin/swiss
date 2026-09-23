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

import { describe, expect, it, beforeAll, afterAll } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { emptyLineText } from "../src/group-logic.js";

/* The group component DOM contract (docs/20 section 4 as revised by docs/35): one shape at
 * two densities. The head is a BAND that leads with chevron, name, count, with the two
 * low-frequency actions after them, so the leading columns keep one stable x for the CSS
 * indent contract; no folder glyph, no guide line. The WHOLE head is draggable - there is
 * no grip - and the two buttons opt out at dragstart. At page density the group is the
 * card itself. The suite runs the real mountGroup under a hand-rolled DOM whose elements
 * keep their children (the navigation suite idiom, extended by one tree walk), plus pure
 * wording and the CSS numbers the head markup and base.css must stay in agreement on. */

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
    listeners: {} as Record<string, ((e: any) => void)[]>,
    setAttribute: function () {},
    getAttribute: function () { return null; },
    appendChild: function (c: any) { node.children.push(c); return c; },
    addEventListener: function (type: string, fn: (e: any) => void) { (node.listeners[type] = node.listeners[type] || []).push(fn); },
    contains: function () { return false; },
    closest: function () { return null; },
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
      // docs/37 R5: the head's glyphs are iconNode() svgs now.
      createElementNS: (_ns: string, tag: string) => fakeNode(tag),
      createDocumentFragment: () => fakeNode("#document-fragment"),
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
  mountGroup = (await import("../src/groups.js")).mountGroup;
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

describe("group head - anatomy", () => {
  it("side: leads with chevron + name + count; the two actions after; no folder, no grip", () => {
    const wrap = mountGroup(cfg([{ name: "redis" }, { name: "mysql" }]), { name: "default", rows: [{ name: "redis" }, { name: "mysql" }] });
    expect(wrap.className).toContain("grp grp--side");
    expect(wrap.className).not.toContain("group");
    const head = wrap.children[0];
    expect(head.className).toBe("grp-head");
    const kinds = head.children.map((c: any) => c.className);
    // The order IS the contract: the leading columns must start at the head first child,
    // because base.css measures the chevron/name x positions from them.
    expect(kinds).toEqual(["grp-toggle", "grp-add", "grp-more"]);
    const toggle = head.children[0];
    const inner = toggle.children.map((c: any) => c.className);
    expect(inner).toEqual(["grp-chev", "grp-name", "grp-n"]);
    expect(toggle.children[1].textContent).toBe("default");
    expect(toggle.children[2].textContent).toBe("2");
  });

  it("page: the group IS the card; the same head, its name over the rows' names", () => {
    const pageCfg: any = cfg([]);
    pageCfg.density = "page";
    pageCfg.rowNode = undefined;
    pageCfg.rowsHtml = () => "<div class='row'></div>";
    pageCfg.rowSel = () => ".row";
    const wrap = mountGroup(pageCfg, { name: "prod", rows: [] });
    // .group is the card class views.css already paints (ring, radius, clip): the group
    // carries it itself instead of nesting a second surface under its head.
    expect(wrap.className).toContain("grp--page");
    expect(wrap.className).toContain("group");
    const toggle = wrap.children[0].children[0];
    const kinds = toggle.children.map((c: any) => c.className);
    expect(kinds).toEqual(["grp-chev", "grp-name", "grp-n"]);
  });

  it("the WHOLE head drags; + and the ellipsis cancel the drag at its start and keep their click", () => {
    let inFlight: string | null = null;
    const c: any = cfg([]);
    c.dragGroup = { get: () => inFlight, set: (v: string | null) => { inFlight = v; } };
    const wrap = mountGroup(c, { name: "default", rows: [] });
    const head = wrap.children[0];
    expect(head.draggable).toBe(true);
    const start = head.listeners.dragstart[0];
    const dt = { setData() {}, effectAllowed: "" };
    // From the name: a group drag begins and the whole group dims.
    let prevented = false;
    start({ target: { closest: () => null }, dataTransfer: dt, preventDefault: () => { prevented = true; } });
    expect(prevented).toBe(false);
    expect(inFlight).toBe("default");
    expect(wrap.classList.contains("dragging")).toBe(true);
    head.listeners.dragend[0]({});
    expect(inFlight).toBe(null);
    expect(wrap.classList.contains("dragging")).toBe(false);
    // From the + button: the drag is cancelled (so the mouse-up is a click) and nothing moves.
    const add = { closest: (sel: string) => (sel.indexOf(".grp-add") >= 0 ? add : null) };
    start({ target: add, dataTransfer: dt, preventDefault: () => { prevented = true; } });
    expect(prevented).toBe(true);
    expect(inFlight).toBe(null);
    expect(wrap.classList.contains("dragging")).toBe(false);
  });

  it("a group drop lands on the WHOLE group (head or members), a row drop on the head or the empty line", () => {
    const c: any = cfg([]);
    c.dragGroup = { get: () => "learn", set: () => {} };
    const wrap = mountGroup(c, { name: "default", rows: [] });
    expect(wrap.listeners.dragover).toHaveLength(1);
    expect(wrap.listeners.drop).toHaveLength(1);
    const head = wrap.children[0];
    const empty = wrap.children[1].children[0];
    expect(empty.className).toBe("grp-empty");
    expect(head.listeners.drop).toHaveLength(1);
    expect(empty.listeners.drop).toHaveLength(1);
    // Another group in flight is accepted here; the group is never a target for itself.
    let allowed = false;
    wrap.listeners.dragover[0]({ clientY: 0, dataTransfer: {}, preventDefault: () => { allowed = true; } });
    expect(allowed).toBe(true);
    const self = mountGroup(c, { name: "learn", rows: [] });
    allowed = false;
    self.listeners.dragover[0]({ clientY: 0, dataTransfer: {}, preventDefault: () => { allowed = true; } });
    expect(allowed).toBe(false);
  });

  it("says the fold state with aria-expanded; a collapsed group keeps its count", () => {
    const open = mountGroup(cfg([]), { name: "default", rows: [] });
    expect(open.className).not.toContain("collapsed");
    const folded = mountGroup(cfg([], { default: true }), { name: "default", rows: [] });
    expect(folded.className).toContain("collapsed");
    expect(folded.children[0].children[0].children[2].textContent).toBe("0");
    // The attribute itself is set through the stubbed setAttribute; pin the source line so
    // the a11y contract cannot drift silently.
    const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../src/groups.ts"), "utf8");
    expect(src).toContain('setAttribute("aria-expanded", String(!folded))');
  });

  it("an empty group keeps its place: one short quiet line", () => {
    const wrap = mountGroup(cfg([]), { name: "default", rows: [] });
    const body = wrap.children[1];
    expect(body.className).toBe("grp-body");
    expect(body.children[0].className).toBe("grp-empty");
    expect(body.children[0].textContent).toBe(emptyLineText(true));
  });

  it("members land in the body; page density writes the rows straight into it", () => {
    const side = mountGroup(cfg([{ name: "redis" }]), { name: "default", rows: [{ name: "redis" }] });
    expect(side.children[1].children[0].dataset.name).toBe("redis");
    const pageCfg: any = cfg([]);
    pageCfg.density = "page";
    // docs/37 R5: the rowsHtml string path retired - the builder's node lands directly,
    // already wired, at BOTH densities. wireRow still runs at page density only.
    let wired = 0;
    pageCfg.rowNode = (row: any) => { const n = fakeNode("div"); n.dataset.name = row.name; return n; };
    pageCfg.wireRow = () => { wired++; };
    const page = mountGroup(pageCfg, { name: "default", rows: [{ name: "redis" }] });
    expect(page.className).toContain("grp--page");
    const body = page.children[1];
    expect(body.className).toBe("grp-body");
    expect(body.children).toHaveLength(1); // the built row, appended not parsed
    expect(body.children[0].dataset.name).toBe("redis");
    expect(wired).toBe(1); // page density wires the caller's actions
    expect(side.children[1].children[0].dataset.name).toBe("redis");
  });
});

describe("empty-line wording (pure)", () => {
  it("stays two words plus at most the drop affordance", () => {
    expect(emptyLineText(true)).toBe("No items — drop here or press +");
    expect(emptyLineText(false)).toBe("No items");
  });
});

describe("the CSS contract (over the shipped sheet)", () => {
  const base = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "styles", "base.css"), "utf8");
  const html = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "index.html"), "utf8");

  it("the head is a band in both densities: --sep-soft ground, no tree rail, no folder glyph", () => {
    expect(base).toMatch(/\n    \.grp-head \{[^}]*background: var\(--sep-soft\)/);
    expect(base).toMatch(/\.grp--side \.grp-head \{ height: 28px/);
    expect(base).toMatch(/\.grp--page \.grp-head \{ height: 36px/);
    expect(base).not.toMatch(/.grp[^ {]*::before/);
    expect(base).not.toContain("grp-folder");
    // fix-plan #14 brought i-folder back to the sprite (the tunnel key picker's directory
    // rows), so the symbol's presence in the shell proves nothing about the tree - the
    // tree's own module must not reference it.
    const groupsSrc = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "groups.ts"), "utf8");
    expect(groupsSrc).not.toContain("i-folder");
  });

  it("members sit one grid step in under the sidebar band; the card's rows run edge to edge", () => {
    // The number behind the label-offset contract in the base.css groups section (a
    // member's dot sits under the band's name): changing it is a deliberate hierarchy
    // change, not a tweak.
    expect(base).toMatch(/\.grp--side \.grp-body \{ padding: var\(--s1\) 0 0 var\(--s4\); \}/);
    expect(base).not.toMatch(/\.grp--page \.grp-body \{ padding/);
  });

  it("the whole head shows the grab cursor, the group dims while dragging, and there is no grip anywhere", () => {
    expect(base).toMatch(/\.grp-head\[draggable="true"\] \{ cursor: grab; \}/);
    expect(base).toMatch(/\.grp\.dragging \{ opacity: 0\.55; \}/);
    expect(base).not.toContain("grp-grip");
    expect(html).not.toContain('id="i-grip"');
  });

  it("drop feedback lands on the whole group for a group drag, on the head or the empty line for a row drag", () => {
    expect(base).toMatch(/\.grp--page\.drop-before \{ box-shadow: var\(--shadow-card\), inset 0 2px 0 var\(--accent\); \}/);
    expect(base).toMatch(/\.grp--side\.drop-after \{ box-shadow: inset 0 -2px 0 var\(--accent\); \}/);
    expect(base).toMatch(/\.grp-head\.drop-into, \.grp-empty\.drop-into \{/);
  });
});
