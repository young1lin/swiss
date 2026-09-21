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

import { describe, it, expect, beforeAll, afterAll } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

/* The tunnels plugin contributes two L2 pages (docs/13 D5, as revised): #tunnels (SSH
   Connections) and #tunnel-forwards (Port Forwards). This suite pins the page split at the
   panel's level — the Rust descriptor's half lives in src/builtin.rs's own tests:
   - both entry modules exist at the paths the descriptors promise;
   - mounting a page pins its scope, so operations built on tunScope() keep targeting the
     right list without the old page-local tab control;
   - the L2 segmented control is gone from the body — the context bar owns page switching;
   - the count text names the mounted page's scope, not the other one's. */

// eslint-disable-next-line @typescript-eslint/no-explicit-any
let mods: any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let tunState: any;

/* docs/37 R5: the render path builds nodes now, so the permissive stub grows the three
   factory calls h()/frag() make (plus the Node identity instanceof checks against). This
   suite never reaches renderTunnels — currentView() stays "mcps", so loadTunnels only
   stores the answer — but a stub that would explode on the first paint hides that fact
   instead of reporting it. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

beforeAll(async () => {
  const prevDocument = Object.getOwnPropertyDescriptor(globalThis, "document");
  const prevFetch = Object.getOwnPropertyDescriptor(globalThis, "fetch");
  const el = (): Record<string, unknown> => ({
    style: {}, dataset: {}, hidden: false, disabled: false, value: "", textContent: "", innerHTML: "",
    title: "", className: "", onclick: null,
    classList: { add() {}, remove() {}, toggle() {}, contains() { return false; } },
    setAttribute() {}, getAttribute() { return ""; }, removeAttribute() {},
    addEventListener() {}, removeEventListener() {},
    appendChild(c: unknown) { return c; }, removeChild(c: unknown) { return c; }, remove() {},
    querySelector() { return null; }, querySelectorAll() { return []; },
    focus() {}, blur() {}, click() {},
    getBoundingClientRect() { return { left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }; },
    contains() { return false; }, closest() { return null; },
  });
  Object.assign(globalThis, {
    document: {
      documentElement: el(), body: el(), head: el(),
      hidden: false, visibilityState: "visible", activeElement: null,
      getElementById: () => el(), createElement: () => el(), createTextNode: () => el(),
      createElementNS: () => el(), createDocumentFragment: () => el(),
      querySelector: () => null, querySelectorAll: () => [],
      addEventListener() {}, removeEventListener() {},
    },
    fetch: async () => ({ ok: true, status: 200, json: async () => ({ connections: [], rules: [], ruleGroups: [], connGroups: [], mcps: [] }) }),
  });
  afterAll(() => {
    if (prevDocument) Object.defineProperty(globalThis, "document", prevDocument);
    else delete (globalThis as Record<string, unknown>).document;
    if (prevFetch) Object.defineProperty(globalThis, "fetch", prevFetch);
    else delete (globalThis as Record<string, unknown>).fetch;
  });
  tunState = await import("../src/tunnel-state.js");
  mods = await import("../src/tunnels.js");
});

describe("the tunnels plugin's two L2 pages", () => {
  it("both entry modules exist at the paths the descriptors promise", () => {
    // The descriptors name SERVED paths (/admin/js/views/...), so this checks the emitted
    // tree the browser loads - the sources behind them are pinned by panel-emit.test.ts.
    const base = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "js", "views");
    expect(existsSync(join(base, "tunnels.js")), "views/tunnels.js").toBe(true);
    expect(existsSync(join(base, "tunnel-forwards.js")), "views/tunnel-forwards.js").toBe(true);
  });

  it("mounting a page pins its scope, so scope-keyed operations follow the page", async () => {
    await mods.mountTunnelsPage("conns");
    expect(tunState.mountedTunScope()).toBe("conns");
    await mods.mountTunnelsPage("rules");
    expect(tunState.mountedTunScope()).toBe("rules");
  });

  it("unmount clears the family's cached state, exactly as the old single view did", () => {
    tunState.setTunResponse({ connections: [{ id: "c1" }], rules: [] });
    tunState.setTunKeys({ keys: [] });
    tunState.setTunDragging("c1");
    mods.unmountTunnelsPage();
    expect(tunState.tunResponse()).toBe(null);
    expect(tunState.tunKeys()).toBe(null);
    expect(tunState.tunDragging()).toBe(null);
  });

  it("isTunnelsView covers exactly the two page ids the guards key on", async () => {
    const polling = await import("../src/polling.js");
    expect(polling.isTunnelsView("tunnels")).toBe(true);
    expect(polling.isTunnelsView("tunnel-forwards")).toBe(true);
    // Neither the plugin id nor any other page id may pass: a too-wide guard would let a
    // foreign view's poll repaint the tunnel body.
    expect(polling.isTunnelsView("mcps")).toBe(false);
    expect(polling.isTunnelsView("data")).toBe(false);
    expect(polling.isTunnelsView("tunnel")).toBe(false);
  });

  it("the count text names the mounted page's scope", () => {
    tunState.setTunResponse({
      connections: [
        { id: "c1", state: "connected" },
        { id: "c2", state: "down" },
        { id: "c3", state: "connected" },
      ],
      rules: [
        { id: "r1", state: "up" },
        { id: "r2", state: "down" },
      ],
      ruleGroups: [], connGroups: [], mcps: [],
    });
    expect(mods.tunnelsCountText("conns")).toBe("3 connections, 2 connected");
    expect(mods.tunnelsCountText("rules")).toBe("2 rules, 1 active");
    // Singular forms stay honest.
    tunState.tunResponse().connections = [{ id: "c1", state: "connected" }];
    expect(mods.tunnelsCountText("conns")).toBe("1 connection, 1 connected");
  });

  it("the body no longer renders a page-local L2 segmented control", () => {
    const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "tunnels.ts"), "utf8");
    // The old .seg tab pair ("SSH Connections | Port Forwards" in the body) is gone; the
    // context bar's page switcher is the one L2 mechanism. The paint is BUILT now (docs/37
    // R5), so the guard covers both spellings a built tree could carry the control in.
    expect(src).not.toContain("tablist");
    expect(src).not.toContain('data: { tab:');
    // The name of the removed control, guarded as a literal: docs/37 R4 gave the state slice a
    // reader for the mounted scope, and it is deliberately NOT called this.
    expect(src).not.toContain("tunTab");
    // The scope's actions stayed in the body header, including the rules-only pair. h()
    // carries the id as a property, so the prop spelling is what the source pins.
    expect(src).toContain("tNewConn");
    expect(src).toContain("tNewRule");
    expect(src).toContain('id: "tStartAll"');
    expect(src).toContain('id: "tStopAll"');
    // And no location title: the context bar says where we are.
    expect(src).not.toContain("pane-title");
  });

  it("the two view modules delegate to the shared module rather than forking it", async () => {
    const base = join(dirname(fileURLToPath(import.meta.url)), "..", "src", "views");
    const conns = readFileSync(join(base, "tunnels.ts"), "utf8");
    const rules = readFileSync(join(base, "tunnel-forwards.ts"), "utf8");
    for (const src of [conns, rules]) {
      expect(src).toContain('mountTunnelsPage');
      expect(src).toContain('tunnelsCountText');
      expect(src).not.toContain("/api/tunnels"); // no duplicated data logic in the views
    }
    expect(conns).toContain('mountTunnelsPage("conns")');
    expect(rules).toContain('mountTunnelsPage("rules")');
  });
});
