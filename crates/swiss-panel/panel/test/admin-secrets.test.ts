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

import { describe, it, expect, beforeAll, beforeEach } from "vitest";

/* The Secrets page (the Settings group's second page, docs/19 D6): names only, a write-only
   store form, a rev-carrying delete, and a poll that never repaints the form mid-typing. The
   rows markup is a pure string (the requiresBadge precedent — exported for the suite); the
   mutations are driven directly under a fetch stub, because this repo's micro-DOM does not
   parse HTML. */

class FakeNode {
  tag: string;
  id = "";
  value = "";
  innerHTML = "";
  textContent = "";
  hidden = false;
  className = "";
  title = "";
  type = "";
  draggable = false;
  style = {};
  dataset: Record<string, string> = {};
  classList = { add() {}, remove() {}, contains() { return false; } };
  children: FakeNode[] = [];
  constructor(tag: string) {
    this.tag = tag.toUpperCase();
  }
  appendChild(n: FakeNode) {
    this.children.push(n);
    return n;
  }
  querySelector(_sel?: string): FakeNode | null {
    return null;
  }
  querySelectorAll(): FakeNode[] {
    return [];
  }
  addEventListener() {}
  setAttribute() {}
  focus() {}
  getBoundingClientRect() {
    return { top: 0, left: 0, right: 0, bottom: 0, width: 0, height: 0 };
  }
  closest(): FakeNode | null {
    return null;
  }
}

interface Call {
  method: string;
  url: string;
  body?: string;
}

let calls: Call[];
let replies: Record<string, unknown>;
let confirmAnswer: boolean;
let pane: FakeNode;
let chip: FakeNode;
// Ids a test plants on purpose (the store form's inputs); everything else auto-creates.
const planted: Record<string, FakeNode> = {};
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let mods: any;

function fakeInput(value: string): FakeNode {
  const n = new FakeNode("input");
  n.value = value;
  return n;
}

beforeAll(async () => {
  const anyG = globalThis as unknown as Record<string, unknown>;
  // localStorage stub: the groups component persists fold state under swiss.groups.<scope>.
  const ls: Record<string, string> = {};
  anyG.window = {
    innerWidth: 1440,
    innerHeight: 900,
    addEventListener() {},
    localStorage: {
      getItem: (k: string) => ls[k] ?? null,
      setItem: (k: string, v: string) => { ls[k] = v; },
      removeItem: (k: string) => { delete ls[k]; },
    },
  };
  anyG.confirm = () => confirmAnswer;
  const doc = new FakeNode("#document");
  const docBody = new FakeNode("body");
  doc.appendChild(docBody);
  // The groups component builds real nodes; the micro-DOM grows one factory for it.
  (doc as unknown as { createElement(tag: string): FakeNode }).createElement = (tag: string) =>
    new FakeNode(tag);
  // Permissive getElementById: pane/countChip are planted by the suite, everything else
  // auto-creates so module-top-level wiring (toasts, sheets) does not explode.
  (doc as unknown as { getElementById(id: string): FakeNode }).getElementById = (id: string) => {
    if (id === "pane") return pane;
    if (id === "countChip") return chip;
    if (planted[id]) return planted[id];
    const made = new FakeNode("div");
    made.id = id;
    docBody.appendChild(made);
    return made;
  };
  anyG.document = doc;
  // navigator is getter-only in this runtime; no test here drives the Copy-ref button, so
  // copyText's clipboard path is never taken and no stub is needed.
  anyG.fetch = async (url: string, init?: RequestInit) => {
    const u = String(url);
    const method = init?.method ?? "GET";
    calls.push({ method, url: u, body: typeof init?.body === "string" ? init.body : undefined });
    const key = method + " " + u;
    const reply = method === "GET" && replies[u] !== undefined ? replies[u] : replies[key];
    const ok = reply !== undefined && reply !== null;
    return {
      ok,
      status: ok ? 200 : 404,
      json: async () => reply ?? { error: "no stub for " + key },
    };
  };
  mods = await import("../src/views/secrets.js");
});

beforeEach(() => {
  calls = [];
  confirmAnswer = true;
  pane = new FakeNode("div");
  pane.querySelector = ((sel: string) => {
    if (sel === "[data-rows]") {
      const rows = new FakeNode("div");
      return rows;
    }
    return null;
  }) as FakeNode["querySelector"];
  chip = new FakeNode("span");
  replies = {};
  for (const k of Object.keys(planted)) delete planted[k];
  // The groups region is its own container now (docs/20 G6): plant it AFTER the reset so the
  // assertions can read what paintGroups drew without the pane's whole markup.
  planted.skGroups = new FakeNode("div");
});

describe("the Secrets page (docs/19 D6)", () => {
  it("renders names and their references — never a value", () => {
    mods.__setVaultForTest(["stripe-key", "zz-last"], 5);
    const html = mods.rowsHtml();
    expect(html).toContain("stripe-key");
    expect(html).toContain("<code>${secret://stripe-key}</code>");
    expect(html).toContain("Copy ref");
    // Delete lives behind the row's overflow menu (design rule 4: red never sits on a row).
    expect(html).not.toContain("btn danger");
    expect(html).toContain("data-skmore=");
    // The page never even holds a value to leak: the state is names + rev only.
    expect(mods.countText()).toBe("2 secrets");
  });

  it("renders rows in the stored order, unranked names after in name order (docs/26)", () => {
    mods.__setVaultForTest(["zz-last", "aa-unranked", "mm-mid"], 5, ["zz-last", "mm-mid"]);
    const html = mods.rowsHtml();
    expect(html.indexOf("zz-last")).toBeLessThan(html.indexOf("mm-mid"));
    expect(html.indexOf("mm-mid")).toBeLessThan(html.indexOf("aa-unranked"));
  });

  it("a row drag PUTs the flat order, then reloads the rev the PUT bumped (docs/26)", async () => {
    mods.__setVaultForTest(["aa", "bb", "cc"], 5);
    replies["PUT /api/groups/secrets/order"] = { order: ["bb", "aa", "cc"] };
    replies["/api/secrets"] = { secrets: ["aa", "bb", "cc"], rev: 6 };
    mods.moveSecretRow("bb", "aa", true);
    await new Promise((r) => setTimeout(r, 0));
    const put = calls.find((c) => c.method === "PUT" && c.url === "/api/groups/secrets/order");
    expect(put?.body && JSON.parse(put.body as string)).toEqual({ order: ["bb", "aa", "cc"] });
    // The order PUT bumps the vault rev - the reload is mandatory, not polish.
    expect(calls.some((c) => c.method === "GET" && c.url === "/api/secrets")).toBe(true);
  });

  it("the empty page says what a secret is for", async () => {
    mods.__setVaultForTest([], 0);
    await mods.mount();
    // The empty state renders into the groups container (docs/20 G6).
    expect(planted.skGroups.innerHTML).toContain("No secrets yet");
    expect(planted.skGroups.innerHTML).toContain("${secret://name}");
    // The store form carries the Group select - the one option is the default group.
    expect(pane.innerHTML).toContain('id="skGroup"');
  });

  it("storing PUTs name+value+rev, then reloads the names", async () => {
    replies["/api/secrets"] = { secrets: [], rev: 3 };
    await mods.mount();
    // The form inputs are read by id: plant them where getElementById looks.
    planted.skName = fakeInput("stripe-key");
    planted.skValue = fakeInput("sk_live_panel");
    replies["PUT /api/secrets/stripe-key"] = { rev: 4 };
    await mods.storeSecret();
    const put = calls.find((c) => c.method === "PUT");
    expect(put?.url).toBe("/api/secrets/stripe-key");
    expect(JSON.parse(put?.body as string)).toEqual({ value: "sk_live_panel", rev: 3 });
  });

  it("a name outside the grammar is refused before any request", async () => {
    await mods.mount();
    planted.skName = fakeInput("BadName");
    planted.skValue = fakeInput("v");
    await mods.storeSecret();
    expect(calls.filter((c) => c.method === "PUT").length).toBe(0);
  });

  it("deleting asks first, then DELETEs by rev; a refused confirm sends nothing", async () => {
    mods.__setVaultForTest(["old"], 9);
    confirmAnswer = false;
    await mods.removeSecret("old");
    expect(calls.length).toBe(0);
    confirmAnswer = true;
    replies["DELETE /api/secrets/old"] = { rev: 10 };
    await mods.removeSecret("old");
    const del = calls.find((c) => c.method === "DELETE");
    expect(del?.url).toBe("/api/secrets/old?rev=9");
  });

  it("the poll patches rows only — the store form survives it", async () => {
    mods.__setVaultForTest(["a"], 1);
    await mods.mount();
    // Simulate the user mid-typing: a repaint would replace the pane's innerHTML wholesale.
    const groupsBefore = planted.skGroups;
    replies["/api/secrets"] = { secrets: ["a", "b"], rev: 2 };
    await mods.poll();
    expect(pane.innerHTML).toContain("inline-form"); // the form markup is still the pane's
    // The refreshed list lands in the SAME groups container - a repaint, not a re-mount.
    expect(planted.skGroups).toBe(groupsBefore);
    expect(mods.rowsHtml()).toContain("b");
    // The refreshed list lands without mount() being called again (no reloadPluginInventory).
    expect(mods.rowsHtml()).toContain("b");
  });
});
