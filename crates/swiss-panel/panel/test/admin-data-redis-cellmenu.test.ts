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
import { dbConn, dbTabs, freshTab, mountDbView, unmountDbView } from "../src/db-state.js";
import type { ApiDbRedisValue, ApiDbStreamWindow } from "../src/types/api.js";

/* docs/43 M2 fixup: the typed value table had NO right-click at all — a zset member that is
 *  a long JSON blob could only be glimpsed through the title hover, and stream values (a
 *  read-only pre) had nothing. The menu is the docs/22 W5.3 vocabulary on the redis side:
 *  copy the cell, or open the same read-only viewer the SQL grid uses. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

type Stub = Record<string, any> & { children: Stub[] };
const el = (tag = "div"): Stub => {
  const n: any = {
    tag, children: [], style: {}, dataset: {}, hidden: false, disabled: false,
    value: "", textContent: "", className: "", id: "", title: "", rows: 0, type: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    appendChild(c: Stub) {
      // Real-DOM semantics: appending a fragment moves its children and empties it; a plain
      // child gains a parent so remove() can actually detach it (popupMenu closes by remove).
      if (c.tag === "#document-fragment") { c.children.forEach((k: Stub) => { k.parentNode = n; n.children.push(k); }); c.children = []; return c; }
      c.parentNode = n; n.children.push(c); return c;
    },
    removeChild(c: Stub) { n.children = n.children.filter((x: Stub) => x !== c); return c; },
    remove() { const p = n.parentNode as Stub | undefined; if (p) p.children = p.children.filter((x: Stub) => x !== n); },
    contains: () => false, closest: () => null,
    setAttribute(k: string, v: string) { if (k === "id") n.id = v; if (k.startsWith("data-")) n.dataset[k.slice(5)] = v; },
    getAttribute: () => "", removeAttribute() {},
    addEventListener() {}, removeEventListener() {},
    dispatchEvent: () => true, focus() {}, blur() {}, select() {}, click() {},
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 40, height: 22, right: 40, bottom: 22 }),
    querySelector: () => null, querySelectorAll: () => [],
    replaceWith() {}, insertAdjacentHTML() {},
  };
  Object.defineProperty(n, "innerHTML", { get: () => "", set: () => {} });
  Object.setPrototypeOf(n, NodeStub.prototype);
  return n;
};
const byId: Record<string, Stub> = {};
const docStub: any = {
  documentElement: el(), body: el(), head: el(), hidden: false, visibilityState: "visible",
  activeElement: null,
  createElement: (t: string) => el(t), createElementNS: (_ns: string, t: string) => el(t),
  createDocumentFragment: () => el("#document-fragment"),
  createTextNode: (s: string) => { const n = el("#text"); n.textContent = s; return n; },
  // byId first (the wiring's own hosts), then the BODY TREE — popupMenu closes by
  // getElementById("menu").remove(), so a menu appended to body must be findable, or
  // every test after the first would click the previous test's menu.
  getElementById: (id: string) => {
    // Body tree FIRST: an attached node (popupMenu's #menu) must win over an auto-created
    // detached stub of the same id, or closeMenu would "remove" the stub and leave the real
    // menu stacked in body for the next test to click. Synthetic hosts (sheet, dbGridWrap)
    // never attach, so they still resolve through byId.
    const walk = (n: Stub): Stub | undefined => {
      if (n.id === id) return n;
      for (const c of n.children || []) { const hit = walk(c); if (hit) return hit; }
      return undefined;
    };
    const attached = walk(docStub.body);
    return attached || byId[id] || (byId[id] = el());
  },
  querySelector: () => null, querySelectorAll: () => [],
  addEventListener: () => {}, removeEventListener: () => {},
};
Object.assign(globalThis, {
  document: docStub,
  window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {}, prompt: () => "",
  setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});
// Node's own global navigator is a getter — defineProperty replaces it; no clipboard
// forces dbCopyText down its fallback path, which the stub document answers.
Object.defineProperty(globalThis, "navigator", { value: {}, configurable: true });

const mod = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-browsers.ts")).href
) as { dbRenderRedisValue: (wrap: Stub) => void };
function find(node: Stub, pred: (n: Stub) => boolean, out: Stub[] = []): Stub[] {
  if (pred(node)) out.push(node);
  for (const c of node.children || []) find(c, pred, out);
  return out;
}
function text(node: Stub): string {
  return (node.textContent || "") + (node.children || []).map(text).join("");
}
function mountValue(v: ApiDbRedisValue | ApiDbStreamWindow): Stub {
  const wrap = el();
  byId.dbGridWrap = wrap;
  byId.sheet = el();
  unmountDbView();
  mountDbView();
  Object.assign(dbConn(), { conn: "r", conns: [{ name: "r", dialect: "redis" }] });
  const k = freshTab("key");
  dbTabs()[0] = k;
  k.redisKey = "z";
  k.redisValue = v;
  mod.dbRenderRedisValue(wrap);
  return wrap;
}
function mountZset(value: unknown, type: string): Stub {
  return mountValue({ key: "z", type: type, value: value, length: 1, ttl: -1 });
}

describe("the typed value table's right-click (docs/43 M2 fixup)", () => {
  it("a zset member cell opens a menu offering copy and the read-only viewer", () => {
    const wrap = mountZset({ "{\"id\":1}": "1767225600" }, "zset");
    const member = find(wrap, (n) => n.tag === "td" && n.textContent === '{\"id\":1}')[0];
    expect(member, "the member cell exists").toBeTruthy();
    expect(typeof member.oncontextmenu, "the cell carries a contextmenu").toBe("function");
    member.oncontextmenu({ preventDefault: () => {}, clientX: 10, clientY: 10 });
    const menu = find(byId.dbGridWrap, () => false).length >= 0 && (globalThis.document as any).body.children.find((c: Stub) => c.id === "menu");
    expect(menu, "the menu opened").toBeTruthy();
    const labels = menu.children.filter((c: Stub) => c.tag === "button").map((b: Stub) => b.textContent);
    expect(labels).toContain("Copy value");
    expect(labels).toContain("View value…");
  });

  it("View opens the value sheet carrying the whole member - the truncated cell, whole", () => {
    // A plain-text member: the viewer's pre guarantees the WHOLE string is in the sheet, which
    // is the complaint being fixed (the cell truncates, the title hover was the only peek).
    const wrap = mountZset({ "a-very-long-member-value-the-cell-truncates": "1767225600" }, "zset");
    const member = find(wrap, (n) => n.tag === "td" && n.textContent === "a-very-long-member-value-the-cell-truncates")[0];
    member.oncontextmenu({ preventDefault: () => {}, clientX: 10, clientY: 10 });
    const menu = (globalThis.document as any).body.children.find((c: Stub) => c.id === "menu");
    const view = menu.children.find((c: Stub) => c.textContent === "View value…");
    view.onclick({ stopPropagation: () => {} });
    expect(byId.sheet.hidden, "the viewer sheet opened").toBe(false);
    expect(text(byId.sheet)).toContain("a-very-long-member-value-the-cell-truncates");
  });

  it("the member's own column is named in the viewer's caption, not a wire word", () => {
    const wrap = mountZset({ "{\"id\":1}": "1767225600" }, "zset");
    const member = find(wrap, (n) => n.tag === "td" && n.textContent === '{\"id\":1}')[0];
    member.oncontextmenu({ preventDefault: () => {}, clientX: 10, clientY: 10 });
    const menu = (globalThis.document as any).body.children.find((c: Stub) => c.id === "menu");
    const view = menu.children.find((c: Stub) => c.textContent === "View value…");
    view.onclick({ stopPropagation: () => {} });
    // en copy for dataBrowsers.colMember is lowercase "member" (zh: 成员) — same word the
    // column header paints, not the wire key "member" or a raw dictionary key.
    expect(text(byId.sheet)).toContain("member");
    expect(text(byId.sheet)).toContain("z · zset");
  });

  it("a stream value's window cells carry the same right-click (docs/45 S2)", () => {
    // The stream view moved off the read-only pre onto its own window table; the docs/22
    // W5.3 cell menu travels with the cells that actually show data now.
    const wrap = mountValue({
      key: "z", type: "stream", ttl: -1, length: 1,
      entries: [{ id: "1690000000000-0", ts: "2023-07-22T04:00:00.000Z", fields: { a: "1" } }],
      columns: ["a"], more: false, firstId: "1690000000000-0", lastId: "1690000000000-0",
    });
    const cell = find(wrap, (n) => n.tag === "td" && n.textContent === "1")[0];
    expect(cell, "the stream field cell exists").toBeTruthy();
    expect(typeof cell.oncontextmenu, "the cell carries a contextmenu").toBe("function");
    cell.oncontextmenu({ preventDefault: () => {}, clientX: 10, clientY: 10 });
    const menu = (globalThis.document as any).body.children.find((c: Stub) => c.id === "menu");
    expect(menu, "the menu opened on the stream cell too").toBeTruthy();
    const labels = menu.children.filter((c: Stub) => c.tag === "button").map((b: Stub) => b.textContent);
    expect(labels).toContain("Copy value");
    expect(labels).toContain("View value…");
  });
});
