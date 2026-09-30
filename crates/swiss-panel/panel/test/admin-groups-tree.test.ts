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

import { describe, expect, it, beforeAll } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { emptyLineText } from "../src/group-logic.js";
import { tr } from "../src/i18n.js";
import type { GroupCfg } from "../src/types/dom.js";
import { sheet } from "./styles.js";

/* The group component DOM contract (SPEC §panel.groups by SPEC §panel.groups): one shape at
 * two densities. The head is a BAND that leads with chevron, name, count, with the two
 * low-frequency actions after them, so the leading columns keep one stable x for the CSS
 * indent contract; no folder glyph, no guide line. The WHOLE head is draggable - there is
 * no grip - and the two buttons opt out at dragstart. At page density the group is the
 * card itself. The suite runs the real mountGroup on a real DOM (happy-dom): since SPEC §panel.ui
 * the markup is ui/group.ts's, built with h(), and the drag, drop, fold, + and ⋯ wiring is
 * driven by dispatched events and clicks - so each assertion reads what a browser would,
 * not what a hand-rolled stub recorded. Plus pure wording and the CSS numbers the head
 * markup and ui.css must stay in agreement on. */

interface Row { name: string }

function shellSkeleton(): string {
  const html = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "index.html"), "utf8");
  const ids = Array.from(html.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map((m) => m[1]);
  return Array.from(new Set(ids)).map((id) => '<div id="' + id + '"></div>').join("");
}

let mountGroup: (cfg: GroupCfg<Row>, g: { name: string; rows: Row[] }) => HTMLElement;

beforeAll(async () => {
  // groups.ts -> add-sheet.ts wires shell handles at import time: the served shell's ids.
  document.body.innerHTML = shellSkeleton();
  mountGroup = (await import("../src/groups.js")).mountGroup;
});

/** A side-density cfg with the drag plumbing stubbed: enough for mountGroup to build, none
 *  of it fired unless a test fires it. rows become button nodes the way sidebar.ts builds them. */
function cfg(rows: Row[], collapsed: Record<string, boolean> = {}): GroupCfg<Row> {
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
    rowNode: (m: Row) => {
      const b = document.createElement("button");
      b.dataset.name = m.name;
      return b;
    },
    rowId: (m: Row) => m.name,
    rowsById: () => rows,
    groupOfRow: () => "default",
    onMoveRow: () => {},
    onAssign: () => {},
  };
}

/** A drag event as the browser raises it: bubbling, cancelable, with a DataTransfer the
 *  handlers write their effect into. */
function dragEvent(type: string, clientY = 0): Event {
  const ev = new MouseEvent(type, { bubbles: true, cancelable: true, clientY });
  Object.defineProperty(ev, "dataTransfer", { value: { setData() {}, effectAllowed: "", dropEffect: "" } });
  return ev;
}

function q<T extends Element = HTMLElement>(root: Element, sel: string): T {
  const n = root.querySelector<T>(sel);
  if (!n) throw new Error("missing " + sel);
  return n;
}

const classesOf = (n: Element): string[] => Array.from(n.children).map((c) => c.className);

describe("group head - anatomy", () => {
  it("side: leads with chevron + name + count; the two actions after; no folder, no grip", () => {
    const wrap = mountGroup(cfg([{ name: "redis" }, { name: "mysql" }]), { name: "default", rows: [{ name: "redis" }, { name: "mysql" }] });
    expect(wrap.className).toBe("grp grp--side");
    expect(wrap.dataset.group).toBe("default");
    const head = wrap.children[0];
    expect(head.className).toBe("grp-head");
    // The order IS the contract: the leading columns must start at the head first child,
    // because ui.css measures the chevron/name x positions from them.
    expect(classesOf(head)).toEqual(["grp-toggle", "grp-add", "grp-more"]);
    const toggle = head.children[0];
    expect(classesOf(toggle)).toEqual(["grp-chev", "grp-name", "grp-n"]);
    expect(toggle.children[1].textContent).toBe("default");
    expect(toggle.children[2].textContent).toBe("2");
    expect(q(head, ".grp-add").getAttribute("aria-label")).toBe("Add an MCP to default");
    expect(head.querySelector("use[href='#i-folder'], .grp-grip")).toBeNull();
  });

  it("page: the group IS the card; the same head, its name over the rows' names", () => {
    const pageCfg = cfg([]);
    pageCfg.density = "page";
    pageCfg.rowNode = undefined;
    const wrap = mountGroup(pageCfg, { name: "prod", rows: [] });
    // .group is the card class ui.css already paints (ring, radius, clip): the group
    // carries it itself instead of nesting a second surface under its head.
    expect(wrap.className).toBe("grp grp--page group");
    expect(classesOf(q(wrap, ".grp-toggle"))).toEqual(["grp-chev", "grp-name", "grp-n"]);
  });

  it("the WHOLE head drags; + and the ellipsis cancel the drag at its start and keep their click", () => {
    let inFlight: string | null = null;
    let added: string | null = null;
    const c = cfg([]);
    c.dragGroup = { get: () => inFlight, set: (v: string | null) => { inFlight = v; } };
    c.onAdd = (g: string) => { added = g; };
    const wrap = mountGroup(c, { name: "default", rows: [] });
    const head = q(wrap, ".grp-head");
    expect(head.draggable).toBe(true);
    // From the name: a group drag begins and the whole group dims.
    const fromName = dragEvent("dragstart");
    q(head, ".grp-name").dispatchEvent(fromName);
    expect(fromName.defaultPrevented).toBe(false);
    expect(inFlight).toBe("default");
    expect(wrap.classList.contains("dragging")).toBe(true);
    head.dispatchEvent(dragEvent("dragend"));
    expect(inFlight).toBe(null);
    expect(wrap.classList.contains("dragging")).toBe(false);
    // From the + or the ⋯ (their glyphs included): the drag is cancelled, so the mouse-up
    // is a click, and nothing moves.
    for (const sel of [".grp-add", ".grp-more svg"]) {
      const fromButton = dragEvent("dragstart");
      q(head, sel).dispatchEvent(fromButton);
      expect(fromButton.defaultPrevented, sel).toBe(true);
      expect(inFlight).toBe(null);
      expect(wrap.classList.contains("dragging")).toBe(false);
    }
    // ...and the + still answers its click, with this group, without folding it.
    q<HTMLButtonElement>(head, ".grp-add").click();
    expect(added).toBe("default");
    expect(c.collapsed).toEqual({});
  });

  it("a group drop lands on the WHOLE group (head or members), a row drop on the head or the empty line", () => {
    const c = cfg([]);
    c.dragGroup = { get: () => "learn", set: () => {} };
    const wrap = mountGroup(c, { name: "default", rows: [] });
    // Another group in flight is accepted anywhere on this one - on the group, and bubbling
    // up from its head and its body.
    for (const target of [wrap, q(wrap, ".grp-head"), q(wrap, ".grp-empty")]) {
      const over = dragEvent("dragover");
      target.dispatchEvent(over);
      expect(over.defaultPrevented).toBe(true);
    }
    // ...and the group is never a target for itself.
    const self = mountGroup(c, { name: "learn", rows: [] });
    const selfOver = dragEvent("dragover");
    self.dispatchEvent(selfOver);
    expect(selfOver.defaultPrevented).toBe(false);

    // A ROW in flight lands INTO the group from the head or from the empty line.
    const assigned: string[] = [];
    const rc = cfg([{ name: "redis" }]);
    rc.drag = { get: () => "redis", set: () => {} };
    rc.onAssign = (id: string, g: string | null) => { assigned.push(id + "->" + g); };
    const learn = mountGroup(rc, { name: "learn", rows: [] });
    for (const sel of [".grp-head", ".grp-empty"]) {
      const target = q(learn, sel);
      const over = dragEvent("dragover");
      target.dispatchEvent(over);
      expect(over.defaultPrevented, sel).toBe(true);
      expect(target.classList.contains("drop-into")).toBe(true);
      const drop = dragEvent("drop");
      target.dispatchEvent(drop);
      expect(drop.defaultPrevented, sel).toBe(true);
      expect(target.classList.contains("drop-into")).toBe(false);
    }
    expect(assigned).toEqual(["redis->learn", "redis->learn"]);
  });

  it("says the fold state with aria-expanded; a collapsed group keeps its count; the toggle folds it", () => {
    const open = mountGroup(cfg([]), { name: "default", rows: [] });
    expect(open.className).not.toContain("collapsed");
    expect(q(open, ".grp-toggle").getAttribute("aria-expanded")).toBe("true");
    const folded = mountGroup(cfg([], { default: true }), { name: "default", rows: [] });
    expect(folded.className).toContain("collapsed");
    expect(q(folded, ".grp-toggle").getAttribute("aria-expanded")).toBe("false");
    expect(q(folded, ".grp-n").textContent).toBe("0");
    // A real click on the toggle flips the stored fold and repaints.
    const c = cfg([]);
    let renders = 0;
    c.render = () => { renders++; };
    const wrap = mountGroup(c, { name: "default", rows: [] });
    q<HTMLButtonElement>(wrap, ".grp-toggle").click();
    expect(c.collapsed).toEqual({ default: true });
    q<HTMLButtonElement>(wrap, ".grp-toggle").click();
    expect(c.collapsed).toEqual({});
    expect(renders).toBe(2);
  });

  it("the ⋯ opens the stock menu: moves that exist here, rename, then delete after a separator", () => {
    const wrap = mountGroup(cfg([]), { name: "default", rows: [] });
    q<HTMLButtonElement>(wrap, ".grp-more").click();
    const menu = document.getElementById("menu");
    expect(menu?.className).toBe("menu float");
    // "default" is first of two: no Move up, a Move down.
    const items = Array.from(menu?.children || []).map((n) => n.tagName === "HR" ? "---" : n.textContent);
    expect(items).toEqual([tr("groups.moveDown"), "---", tr("groups.rename"), "---", tr("groups.deleteGroup")]);
    expect(menu?.lastElementChild?.className).toBe("danger");
    menu?.remove();
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
    const sideRow = side.children[1].children[0] as HTMLElement;
    expect(sideRow.dataset.name).toBe("redis");
    expect(sideRow.draggable).toBe(true);
    const pageCfg = cfg([]);
    pageCfg.density = "page";
    // SPEC §panel.toolchain: the rowsHtml string path retired - the builder's node lands directly,
    // already wired, at BOTH densities. wireRow still runs at page density only.
    let wired = 0;
    pageCfg.rowNode = (row: Row) => { const n = document.createElement("div"); n.dataset.name = row.name; return n; };
    pageCfg.wireRow = () => { wired++; };
    const page = mountGroup(pageCfg, { name: "default", rows: [{ name: "redis" }] });
    expect(page.className).toContain("grp--page");
    const body = page.children[1];
    expect(body.className).toBe("grp-body");
    expect(body.children).toHaveLength(1); // the built row, appended not parsed
    expect((body.children[0] as HTMLElement).dataset.name).toBe("redis");
    expect(wired).toBe(1); // page density wires the caller's actions
  });
});

describe("empty-line wording (pure)", () => {
  it("stays two words plus at most the drop affordance", () => {
    expect(emptyLineText(true)).toBe("No items — drop here or press +");
    expect(emptyLineText(false)).toBe("No items");
  });
});

describe("the CSS contract (over the shipped sheet)", () => {
  // The groups component's rules live in the component layer since SPEC §panel.ui.
  const base = sheet("ui.css");
  const html = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "index.html"), "utf8");

  it("the head is a band in both densities: --sep-soft ground, no tree rail, no folder glyph", () => {
    expect(base).toMatch(/\n    \.grp-head \{[^}]*background: var\(--sep-soft\)/);
    expect(base).toMatch(/\.grp--side \.grp-head \{ height: 28px/);
    expect(base).toMatch(/\.grp--page \.grp-head \{ height: 36px/);
    expect(base).not.toMatch(/.grp[^ {]*::before/);
    expect(base).not.toContain("grp-folder");
    // SPEC §panel.design brought i-folder back to the sprite (the tunnel key picker's directory
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
