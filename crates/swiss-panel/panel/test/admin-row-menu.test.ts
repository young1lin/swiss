/*
 * Copyright 2026 The swiss authors
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

import { describe, it, expect, beforeAll } from "vitest";

/* Integration: the row overflow menus (docs/18 V5) against the document-level click closer in
   connect.js. Regression (2026-09-12): the Jobs/Tunnels row ellipsis wired popupMenu WITHOUT
   stopping propagation, so the very click that opened the menu bubbled on to document, where
   connect.js's closer saw the menu latch and tore the menu down — a click, no menu, no error.
   The pane's own overflow button and the tunnel group-head menus never had this because they
   call ev.stopPropagation() first.

   There is no DOM library in this repo, so this suite drives the real modules (wireJobs /
   wireTunnels -> menu.js popupMenu -> pane.js closeMenu -> connect.js's document closer) under
   a micro-DOM whose click events REALLY BUBBLE — the bug only exists in the presence of
   bubbling, so a fake without it cannot catch it. */

interface FakeEvent {
  type: string;
  target: FakeNode;
  stopped: boolean;
  stopPropagation(): void;
  preventDefault(): void;
}

class FakeNode {
  tag: string;
  attrs: Record<string, string> = {};
  children: FakeNode[] = [];
  parent: FakeNode | null = null;
  className = "";
  textContent = "";
  innerHTML = "";
  id = "";
  type = "button"; // sheet/form code sets .type on buttons
  onclick: ((ev: FakeEvent) => void) | null = null;
  listeners: Record<string, Array<(e: FakeEvent) => void>> = {};
  style: Record<string, string> = {};
  hidden = false;
  /** Only the document node ever sets this; typed so doc.body.appendChild narrows cleanly. */
  body: FakeNode | null = null;

  constructor(tag: string) {
    this.tag = tag.toUpperCase();
  }

  get dataset(): Record<string, string> {
    const d: Record<string, string> = {};
    for (const k of Object.keys(this.attrs)) {
      if (k.startsWith("data-")) d[k.slice(5).replace(/-([a-z])/g, (_m, c) => c.toUpperCase())] = this.attrs[k];
    }
    return d;
  }

  setAttribute(k: string, v: string) {
    this.attrs[k] = v;
    if (k === "id") this.id = v;
  }
  getAttribute(k: string) {
    return Object.prototype.hasOwnProperty.call(this.attrs, k) ? this.attrs[k] : null;
  }
  appendChild(n: FakeNode) {
    n.remove();
    n.parent = this;
    this.children.push(n);
    return n;
  }
  removeChild(n: FakeNode) {
    const i = this.children.indexOf(n);
    if (i >= 0) this.children.splice(i, 1);
    n.parent = null;
    return n;
  }
  remove() {
    if (this.parent) this.parent.removeChild(this);
  }
  contains(n: FakeNode): boolean {
    return n === this || this.children.some((c) => c.contains(n));
  }
  addEventListener(t: string, fn: (e: FakeEvent) => void) {
    (this.listeners[t] ||= []).push(fn);
  }
  select() {} /* the clipboard textarea path in copyText */
  focus() {}
  getBoundingClientRect() {
    return { left: 100, right: 160, top: 100, bottom: 132, width: 60, height: 32 };
  }

  /** One compound token, e.g. "button", "#menu", ".danger", "button.danger", "div[data-x]" —
   *  every segment must match (tag + class/attr filters glued together). */
  matchesSimple(part: string): boolean {
    const segs = part.match(/[a-zA-Z][a-zA-Z0-9-]*|#[^.#[]+|\.[^.#[]+|\[[^\]]+\]/g) || [];
    return segs.every((s) => {
      if (s[0] === "#") return this.id === s.slice(1);
      if (s[0] === ".") return this.className.split(/\s+/).includes(s.slice(1));
      if (s[0] === "[") return this.getAttribute(s.slice(1, -1)) !== null;
      return this.tag === s.toUpperCase();
    });
  }
  matches(sel: string): boolean {
    return sel.split(",").some((p) => {
      const parts = p.trim().split(/\s+/);
      // Last part matches this node; each earlier part must be matched by some ancestor.
      if (!this.matchesSimple(parts[parts.length - 1])) return false;
      let anc = this.parent;
      for (let i = parts.length - 2; i >= 0; i--) {
        while (anc && !anc.matchesSimple(parts[i])) anc = anc.parent;
        if (!anc) return false;
        anc = anc.parent;
      }
      return true;
    });
  }
  closest(sel: string): FakeNode | null {
    let n: FakeNode | null = this;
    while (n) {
      if (n.matches(sel)) return n;
      n = n.parent;
    }
    return null;
  }
  querySelectorAll(sel: string): FakeNode[] {
    const out: FakeNode[] = [];
    const walk = (n: FakeNode) => {
      if (n !== this && n.matches(sel)) out.push(n);
      n.children.forEach(walk);
    };
    walk(this);
    return out;
  }
  querySelector(sel: string): FakeNode | null {
    return this.querySelectorAll(sel)[0] || null;
  }
}

const doc = new FakeNode("#document");
const docBody = new FakeNode("body");
doc.body = docBody;
doc.appendChild(doc.body);
(doc as unknown as { getElementById(id: string): FakeNode }).getElementById = (id: string) => {
  const walk = (n: FakeNode): FakeNode | null => {
    if (n.id === id) return n;
    for (const c of n.children) {
      const hit = walk(c);
      if (hit) return hit;
    }
    return null;
  };
  const hit = walk(doc);
  if (hit) return hit;
  // Permissive: ids the shell owns but this micro-DOM never builds (addBtn, sheet, toasts...)
  // still need to exist so module-top-level wiring does not explode.
  const made = new FakeNode("div");
  made.id = id;
  docBody.appendChild(made);
  return made;
};
(doc as unknown as { querySelector(sel: string): FakeNode | null }).querySelector = (sel: string) =>
  (doc as unknown as { querySelectorAll(s: string): FakeNode[] }).querySelectorAll.call(doc, sel)[0] || null;
(doc as unknown as { querySelectorAll(sel: string): FakeNode[] }).querySelectorAll = (sel: string) => {
  const out: FakeNode[] = [];
  const walk = (n: FakeNode) => {
    if (n !== doc && n.matches(sel)) out.push(n);
    n.children.forEach(walk);
  };
  walk(doc);
  return out;
};
(doc as unknown as { createElement(t: string): FakeNode }).createElement = (t: string) => new FakeNode(t);

const documentCloserCalls: string[] = [];

/** A click that really bubbles: target handler first, then each ancestor's, honoring
 *  stopPropagation exactly like the browser. */
function click(node: FakeNode) {
  const ev: FakeEvent = {
    type: "click",
    target: node,
    stopped: false,
    stopPropagation() {
      this.stopped = true;
    },
    preventDefault() {},
  };
  let n: FakeNode | null = node;
  while (n) {
    if (typeof n.onclick === "function") n.onclick(ev);
    for (const fn of n.listeners.click || []) {
      if (n === doc) documentCloserCalls.push("click");
      fn(ev);
    }
    if (ev.stopped) break;
    n = n.parent;
  }
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
let mods: any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let tunState: any;
let jobState: any;

const JOB_RUNS_URL = "/api/jobs/env-check/runs";

beforeAll(async () => {
  const anyG = globalThis as unknown as Record<string, unknown>;
  anyG.window = { innerWidth: 1440, innerHeight: 900, addEventListener() {} };
  anyG.confirm = () => true;
  anyG.fetch = async (url: string) => ({
    ok: true,
    status: 200,
    json: async () => {
      if (String(url).includes(JOB_RUNS_URL)) return { runs: [], nextBefore: null };
      if (String(url).startsWith("/api/jobs")) return { jobs: [] };
      return {};
    },
  });
  anyG.document = doc;

  // connect.js installs the document-level click closer — the code the bug lived under.
  await import("../src/connect.js");
  mods = {
    jobs: await import("../src/jobs.js"),
    tunnels: await import("../src/tunnels.js"),
    util: await import("../src/util.js"),
  };
  tunState = await import("../src/tunnel-state.js");
  jobState = await import("../src/job-state.js");
});

describe("row overflow menu vs the document click closer (docs/18 V5)", () => {
  function actionButton(attr: string): FakeNode {
    const b = new FakeNode("button");
    b.setAttribute(attr, "");
    b.className = "btn ghost icon";
    return b;
  }
  function paneNode(): FakeNode {
    const get = (doc as unknown as { getElementById(id: string): FakeNode }).getElementById("pane");
    get.children.length = 0;
    get.innerHTML = "";
    return get;
  }
  function openMenuOf(button: FakeNode): FakeNode {
    click(button);
    // querySelector, not the permissive getElementById (which auto-creates): an eaten menu
    // must read as null here.
    const menu = doc.querySelector("#menu");
    expect(menu).toBeTruthy();
    return menu!;
  }
  const labelsOf = (menu: FakeNode) => menu.querySelectorAll("button").map((b) => b.textContent);

  /** One Jobs row against the real jobRowHtml contract, wired by the real wireJobs. */
  function wiredJobRow(): FakeNode {
    const pane = paneNode();
    const row = new FakeNode("div");
    row.setAttribute("data-job", "env-check");
    row.appendChild(actionButton("data-run"));
    row.appendChild(actionButton("data-more"));
    pane.appendChild(row);
    // jobByName() reads the jobs domain rows — the payload loadJobs stores.
    jobState.setJobRows([{ name: "env-check", cron: "5 * * * *", enabled: true, editableInV1: true }]);
    jobState.clearJobBusy("env-check");
    mods.jobs.wireJobs();
    return row;
  }

  it("a Jobs row ellipsis opens its menu even though the click bubbles to document", () => {
    const row = wiredJobRow();
    // THE regression: without stopPropagation the same click that opens the menu reaches
    // connect.js's closer, which removes it before the browser even paints.
    const menu = openMenuOf(row.querySelector("[data-more]")!);
    expect(labelsOf(menu)).toEqual(["Edit", "History", "Delete"]);
    expect(menu.querySelectorAll("button.danger").map((b) => b.textContent)).toEqual(["Delete"]);
  });

  it("an outside click still closes an open menu (the closer itself must keep working)", () => {
    const row = wiredJobRow();
    openMenuOf(row.querySelector("[data-more]")!);
    expect(doc.querySelector("#menu")).toBeTruthy();
    click(docBody);
    expect(doc.querySelector("#menu")).toBe(null);
  });

  it("History from the menu opens the runs sheet for that job", async () => {
    const row = wiredJobRow();
    const menu = openMenuOf(row.querySelector("[data-more]")!);
    const hist = labelsOf(menu).indexOf("History");
    click(menu.querySelectorAll("button")[hist]);
    await new Promise((r) => setTimeout(r, 0));
    const sheet = (doc as unknown as { getElementById(id: string): FakeNode }).getElementById("sheet");
    expect(sheet.innerHTML).toContain("Runs — env-check");
    // The menu closed behind the action.
    expect(doc.querySelector("#menu")).toBe(null);
  });

  it("a Tunnels rule ellipsis opens its menu; Force free appears only while a port is held", () => {
    const pane = paneNode();
    const held = new FakeNode("div");
    held.setAttribute("data-rule", "r1");
    held.appendChild(actionButton("data-act"));
    held.appendChild(actionButton("data-more"));
    const free = new FakeNode("div");
    free.setAttribute("data-rule", "r2");
    free.appendChild(actionButton("data-act"));
    free.appendChild(actionButton("data-more"));
    pane.appendChild(held);
    pane.appendChild(free);

    // tunData() reads the tunnel domain payload — the payload loadTunnels stores.
    tunState.setMountedTunScope("rules");
    tunState.setTunResponse({
      connections: [],
      rules: [
        { id: "r1", localPort: 50383, portOwner: { pid: 12188 }, host: "a", state: "down" },
        { id: "r2", localPort: 50400, host: "b", state: "down" },
      ],
      ruleGroups: [],
      connGroups: [],
      mcps: [],
    });
    mods.tunnels.wireTunnels();

    const menu1 = openMenuOf(held.querySelector("[data-more]")!);
    expect(labelsOf(menu1)).toEqual(["Edit", "Copy local port", "Force free 50383", "Delete"]);

    click(docBody); // close before opening the next
    const menu2 = openMenuOf(free.querySelector("[data-more]")!);
    expect(labelsOf(menu2)).toEqual(["Edit", "Copy local port", "Delete"]);
  });

  it("a Tunnels connection ellipsis opens its menu", () => {
    const pane = paneNode();
    const conn = new FakeNode("div");
    conn.setAttribute("data-conn", "c1");
    conn.appendChild(actionButton("data-test"));
    conn.appendChild(actionButton("data-more"));
    pane.appendChild(conn);

    tunState.setMountedTunScope("conns");
    tunState.setTunResponse({
      connections: [{ id: "c1", host: "192.168.1.24", port: 22 }],
      rules: [],
      ruleGroups: [],
      connGroups: [],
      mcps: [],
    });
    mods.tunnels.wireTunnels();

    const menu = openMenuOf(conn.querySelector("[data-more]")!);
    expect(labelsOf(menu)).toEqual(["Edit", "Copy host", "Delete"]);
  });
});
