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

import { describe, it, expect, beforeAll, beforeEach } from "vitest";

/* docs/27 §4/§4.1.1: the connection sheet's Advanced fold (proxy + jump) and the transport
   tags on the connection row. The wire half lives in manager.rs's rows test — here the
   panel reads the row fields the backend now carries: the fold renders collapsed with the
   bare word when nothing is configured, chips when something is, the interior echoes the
   stored values (proxyPassword as the mask sentinel), the save payload carries the optional
   keys only while the sheet holds a value, a refused save lands inline with the sheet still
   open, and the row paints proxy / via <name> tags.

   Same approach as admin-row-menu.test.ts: a micro-DOM whose getElementById is permissive
   and persistent, plus a fetch recorder — the modules under test are the real ES modules. */

interface FakeEvent {
  type: string;
  target: FakeNode;
  stopped: boolean;
  stopPropagation(): void;
  preventDefault(): void;
}

/* docs/37 R5: the sheets paint with h()/fill() now, so the fake DOM is a Node-extending
   class whose innerHTML READS serialise the built tree, textContent = "" wipes it, and a
   #document-fragment append splices its children in — the pattern the remote suites use. */
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

class FakeNode extends NodeStub {
  tag: string;
  id = "";
  attrs: Record<string, string> = {};
  children: FakeNode[] = [];
  parent: FakeNode | null = null;
  className = "";
  _text = "";
  get textContent(): string { return this._text; }
  set textContent(v: string) { if (v === "") { this.children = []; this._html = ""; } this._text = v; }
  value = "";
  placeholder = "";
  checked = false;
  selected = false;
  hidden = false;
  disabled = false;
  type = "button";
  style: Record<string, string> = {};
  dataset: Record<string, string> = {};
  onclick: ((ev: FakeEvent) => void) | null = null;
  onchange: ((ev: FakeEvent) => void) | null = null;
  body: FakeNode | null = null;
  private _html = "";

  constructor(tag: string) {
    super();
    this.tag = tag.toUpperCase();
  }

  get innerHTML(): string { return this._html || serialize(this); }
  set innerHTML(v: string) { this._html = v; this.children = []; }
  setAttribute(k: string, v: string) {
    this.attrs[k] = v;
    if (k === "id") this.id = v;
    if (k.startsWith("data-")) this.dataset[k.slice(5)] = v;
  }
  getAttribute(k: string) { return this.attrs[k] ?? null; }
  appendChild(n: FakeNode) {
    if (n.tag === "#DOCUMENT-FRAGMENT") { n.children.forEach((c) => { c.parent = this; this.children.push(c); }); this._html = ""; return n; }
    n.parent = this;
    this.children.push(n);
    this._html = "";
    return n;
  }
  removeChild(n: FakeNode) { return n; }
  remove() {}
  addEventListener() {}
  removeEventListener() {}
  select() {}
  focus() {}
  contains() { return false; }
  closest(sel: string): FakeNode | null { return sel.charAt(0) === "#" && this.id === sel.slice(1) ? this : null; }
  querySelector(): FakeNode | null { return null; }
  querySelectorAll(): FakeNode[] { return []; }
  getBoundingClientRect() { return { left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }; }
}

function serialize(node: FakeNode): string {
  if (!node.tag || node.tag === "#TEXT") return String(node._text ?? "");
  const attrs = Object.keys(node.attrs).map((k) => { return " " + k + '="' + node.attrs[k] + '"'; }).join("");
  const id = node.id ? ' id="' + node.id + '"' : "";
  const cls = node.className ? ' class="' + node.className + '"' : "";
  const dis = node.disabled ? " disabled" : "";
  const hid = node.hidden ? " hidden" : "";
  const val = node.value ? ' value="' + node.value + '"' : "";
  const sel = node.selected ? " selected" : "";
  const kids = node.children.map((c) => { return serialize(c); }).join("");
  const tag = node.tag.toLowerCase();
  return "<" + tag + id + cls + attrs + dis + hid + val + sel + ">" + (kids || node._text) + "</" + tag + ">";
}

const doc = new FakeNode("#document");
const docBody = new FakeNode("body");
doc.body = docBody;
doc.appendChild(docBody);
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
  // Permissive: ids the shell owns but this micro-DOM never builds still need to exist.
  const made = new FakeNode("div");
  made.id = id;
  docBody.appendChild(made);
  return made;
};
(doc as unknown as { createElement(t: string): FakeNode }).createElement = (t: string) => new FakeNode(t);
(doc as unknown as { createElementNS(ns: string, t: string): FakeNode }).createElementNS = (_ns: string, t: string) => new FakeNode(t);
(doc as unknown as { createDocumentFragment(): FakeNode }).createDocumentFragment = () => new FakeNode("#document-fragment");
(doc as unknown as { createTextNode(s: string): FakeNode }).createTextNode = (s: string) => {
  const n = new FakeNode("#text");
  n.textContent = s;
  return n;
};
(doc as unknown as { querySelector(): null }).querySelector = () => null;
(doc as unknown as { querySelectorAll(): FakeNode[] }).querySelectorAll = () => [];

interface FetchCall { url: string; method: string; body: string }
const fetchCalls: FetchCall[] = [];
// Responses queued for the next save request; exhausted falls through to the ok default.
const saveResponses: Array<{ ok: boolean; status: number; error?: string }> = [];

const MASK = "\u2022".repeat(8);

// eslint-disable-next-line @typescript-eslint/no-explicit-any
let mods: any;
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let tunState: any;

beforeAll(async () => {
  const anyG = globalThis as unknown as Record<string, unknown>;
  anyG.window = { innerWidth: 1440, innerHeight: 900, addEventListener() {} };
  anyG.confirm = () => true;
  anyG.fetch = async (url: string, opts: { method?: string; body?: string } = {}) => {
    const u = String(url);
    fetchCalls.push({ url: u, method: opts.method || "GET", body: opts.body || "" });
    if (u.includes("/api/tunnels/keys")) {
      return { ok: true, status: 200, json: async () => ({ keys: [], defaultPath: "C:/users/u/.ssh/id_rsa" }) };
    }
    if ((u === "/api/tunnels/connections" || /\/api\/tunnels\/connections\/.+$/.test(u)) && saveResponses.length) {
      const r = saveResponses.shift()!;
      return { ok: r.ok, status: r.status, json: async () => (r.error ? { error: r.error } : {}) };
    }
    return {
      ok: true,
      status: 200,
      json: async () => ({ connections: [], rules: [], ruleGroups: [], connGroups: [], mcps: [] }),
    };
  };
  anyG.document = doc;
  tunState = await import("../src/tunnel-state.js");
  mods = {
    polling: await import("../src/polling.js"),
    sheets: await import("../src/tunnel-sheets.js"),
  };
});

function seedTunData(connections: Array<Record<string, unknown>>) {
  tunState.setMountedTunScope("conns");
  tunState.setTunResponse({ connections, rules: [], ruleGroups: [], connGroups: ["default"], mcps: [] });
}

function openSheet(def: Record<string, unknown> | null): string {
  mods.sheets.openConnSheet(def);
  const sheet = (doc as unknown as { getElementById(id: string): FakeNode }).getElementById("sheet");
  return sheet.innerHTML;
}

/** The painted tree, by id — the nodes fill() built, not the permissive fallback. */
function get(id: string): FakeNode {
  return (doc as unknown as { getElementById(k: string): FakeNode }).getElementById(id);
}

/** One select's options as { value, text, selected } — what the dropdown tests read. The
 *  label is the option's text child: a built node keeps it there, not on textContent. */
function opt(o: FakeNode): { value: string; text: string; selected: boolean } {
  return { value: o.value, text: o.children.map((c) => { return c.textContent; }).join(""), selected: o.selected };
}

function setVal(id: string, value: string) {
  (doc as unknown as { getElementById(id: string): FakeNode }).getElementById(id).value = value;
}

/** Fill the sheet the way a user would before Save: key auth, all Advanced fields seedable. */
function fillSheet(advanced: { proxy?: string; user?: string; pass?: string; jump?: string }) {
  setVal("c-name", "db");
  setVal("c-host", "10.0.0.9");
  setVal("c-port", "22");
  setVal("c-user", "u");
  setVal("c-auth", "key");
  setVal("c-keypath", "C:/keys/id_rsa");
  setVal("c-pass", "");
  setVal("c-proxy", advanced.proxy || "");
  setVal("c-proxy-user", advanced.user || "");
  setVal("c-proxy-pass", advanced.pass || "");
  setVal("c-jump", advanced.jump || "");
}

function lastSaveBody(): Record<string, unknown> {
  const call = fetchCalls.filter((c) => c.url.includes("/api/tunnels/connections")).pop();
  expect(call, "a save request fired").toBeTruthy();
  return JSON.parse(call!.body);
}

describe("the connection sheet's Advanced fold (docs/27 §4)", () => {
  beforeEach(() => {
    fetchCalls.length = 0;
    saveResponses.length = 0;
  });

  it("renders collapsed, with the bare word and no chips, when nothing is configured", () => {
    seedTunData([{ id: "c1", name: "bastion" }]);
    const html = openSheet(null);
    // No open attribute on the details element: the browser starts it folded. A built tree
    // answers that on the node itself (docs/37 R5), not on a markup string.
    const advanced = get("c-advanced");
    expect(advanced, "the Advanced fold").toBeTruthy();
    expect(advanced.className).toBe("fold");
    expect(advanced.getAttribute("open")).toBe(null);
    expect(html).toContain("<summary>Advanced</summary>");
    // Nothing configured -> no tags at all; the fold line is the sheet's only new element.
    expect(html).not.toContain('<span class="tag">');
    // The inline error line starts hidden.
    const err = get("c-err");
    expect(err.hidden).toBe(true);
    // A refusal from its first paint (hint({ bad })): the red is the class, not an inline style.
    expect(err.className).toBe("hint bad");
  });

  it("folds with proxy / via chips when the connection is configured, still collapsed", () => {
    seedTunData([
      { id: "c1", name: "bastion" },
      { id: "c2", name: "db" },
    ]);
    const html = openSheet({
      id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key",
      proxy: "socks5://127.0.0.1:7890", jump: "c1",
    });
    expect(html).toContain('<span class="tag">proxy</span>');
    expect(html).toContain('<span class="tag">via bastion</span>');
    expect(get("c-advanced").getAttribute("open")).toBe(null);
    // A proxy alone paints only its own chip.
    const proxyOnly = openSheet({
      id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key",
      proxy: "socks5://127.0.0.1:7890",
    });
    expect(proxyOnly).toContain('<span class="tag">proxy</span>');
    expect(proxyOnly).not.toContain('<span class="tag">via');
  });

  it("renders the proxy trio and jump dropdown with the stored values echoed", () => {
    seedTunData([
      { id: "c1", name: "bastion" },
      { id: "c2", name: "db" },
    ]);
    openSheet({
      id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key",
      proxy: "socks5://127.0.0.1:7890", proxyUsername: "pxuser", proxyPassword: MASK, jump: "c1",
    });
    // h() carries the stored values as PROPERTIES (docs/37 R5), so the echo is asserted on
    // the node - the attribute order a string builder left behind is not a contract.
    expect(get("c-proxy").value).toBe("socks5://127.0.0.1:7890");
    expect(get("c-proxy").placeholder).toBe("socks5://127.0.0.1:7890");
    expect(get("c-proxy-user").value).toBe("pxuser");
    // The sentinel rides into the input like the MCP sheet's password: masked, and sent
    // back untouched so the server keeps the stored secret.
    expect(get("c-proxy-pass").type).toBe("password");
    expect(get("c-proxy-pass").value).toBe(MASK);
    // The dropdown: None first, every other connection offered, self excluded, jump picked.
    expect(get("c-jump").children.map(opt)).toEqual([
      { value: "", text: "None", selected: false },
      { value: "c1", text: "bastion", selected: true },
    ]);
  });

  it("saves the optional keys only while the sheet holds a value", async () => {
    seedTunData([{ id: "c1", name: "bastion" }]);
    openSheet(null);
    fillSheet({});
    await mods.sheets.saveConn(null);
    const body = lastSaveBody();
    expect(body.proxy).toBeUndefined();
    expect(body.proxyUsername).toBeUndefined();
    expect(body.proxyPassword).toBeUndefined();
    expect(body.jump).toBeUndefined();

    openSheet(null);
    fillSheet({ proxy: "socks5://127.0.0.1:7890", user: "pxuser", jump: "c1" });
    await mods.sheets.saveConn(null);
    const full = lastSaveBody();
    expect(full.proxy).toBe("socks5://127.0.0.1:7890");
    expect(full.proxyUsername).toBe("pxuser");
    expect(full.jump).toBe("c1");
    // An empty password field is still omitted — clearing means unset.
    expect(full.proxyPassword).toBeUndefined();
  });

  it("echoes an untouched sentinel proxyPassword back so the stored secret survives", async () => {
    seedTunData([{ id: "c1", name: "bastion" }]);
    openSheet(null);
    fillSheet({ pass: MASK });
    await mods.sheets.saveConn(null);
    expect(lastSaveBody().proxyPassword).toBe(MASK);
  });

  it("lands a refused save inline and keeps the sheet open", async () => {
    seedTunData([
      { id: "c1", name: "bastion" },
      { id: "c2", name: "db" },
    ]);
    openSheet({ id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key", jump: "c1" });
    fillSheet({ jump: "c1" });
    saveResponses.push({ ok: false, status: 400, error: "jump cycle detected: c2 -> c1 -> c2" });
    await mods.sheets.saveConn({ id: "c2" });
    const err = get("c-err");
    expect(err.hidden).toBe(false);
    expect(err.textContent).toContain("jump cycle detected");
    expect(err.className).toBe("hint bad");
    // The sheet stays open — the fix is a keystroke away, not a re-open.
    const sheet = get("sheet");
    expect(sheet.hidden).toBe(false);
    expect(sheet.innerHTML).not.toBe("");
  });
});

describe("the connection row's transport tags (docs/27 §4)", () => {
  it("paints proxy / via <name> and nothing for a plain row", () => {
    seedTunData([
      { id: "c1", name: "bastion" },
      { id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key", state: "idle" },
    ]);
    // The row is a built node now (docs/37 R5); serialize reads it back as markup.
    const plain = serialize(mods.polling.connRowNode({ id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key", state: "idle" }));
    expect(plain).not.toContain('<span class="tag">');

    const proxied = serialize(mods.polling.connRowNode({ id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key", state: "idle", proxy: "socks5://127.0.0.1:7890" }));
    expect(proxied).toContain('<span class="tag">proxy</span>');
    expect(proxied).not.toContain('<span class="tag">via');

    const jumping = serialize(mods.polling.connRowNode({ id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key", state: "idle", jump: "c1" }));
    expect(jumping).toContain('<span class="tag">via bastion</span>');
  });
});
