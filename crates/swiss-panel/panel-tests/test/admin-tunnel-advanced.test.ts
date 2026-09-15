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

class FakeNode {
  tag: string;
  id = "";
  attrs: Record<string, string> = {};
  children: FakeNode[] = [];
  parent: FakeNode | null = null;
  className = "";
  textContent = "";
  innerHTML = "";
  value = "";
  checked = false;
  hidden = false;
  disabled = false;
  type = "button";
  style: Record<string, string> = {};
  onclick: ((ev: FakeEvent) => void) | null = null;
  onchange: ((ev: FakeEvent) => void) | null = null;
  body: FakeNode | null = null;

  constructor(tag: string) {
    this.tag = tag.toUpperCase();
  }

  get dataset(): Record<string, string> { return {}; }
  setAttribute(k: string, v: string) {
    this.attrs[k] = v;
    if (k === "id") this.id = v;
  }
  getAttribute(k: string) { return this.attrs[k] ?? null; }
  appendChild(n: FakeNode) { n.parent = this; this.children.push(n); return n; }
  removeChild(n: FakeNode) { return n; }
  remove() {}
  addEventListener() {}
  removeEventListener() {}
  select() {}
  focus() {}
  contains() { return false; }
  closest() { return null; }
  querySelector() { return null; }
  querySelectorAll(): FakeNode[] { return []; }
  getBoundingClientRect() { return { left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }; }
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
let state: any;

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
  state = (await import("../../src/admin_assets/js/util.js")).state;
  mods = {
    polling: await import("../../src/admin_assets/js/polling.js"),
    sheets: await import("../../src/admin_assets/js/tunnel-sheets.js"),
  };
});

function seedTunData(connections: Array<Record<string, unknown>>) {
  state.tun.tab = "conns";
  state.tun.data = { connections, rules: [], ruleGroups: [], connGroups: ["default"], mcps: [] };
}

function openSheet(def: Record<string, unknown> | null): string {
  mods.sheets.openConnSheet(def);
  const sheet = (doc as unknown as { getElementById(id: string): FakeNode }).getElementById("sheet");
  return sheet.innerHTML;
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
    // No open attribute anywhere on the details tag: the browser starts it folded.
    expect(html).toContain('<details class="fold" id="c-advanced">');
    expect(html).not.toMatch(/<details[^>]*\sopen[\s>]/);
    expect(html).toContain("<summary>Advanced</summary>");
    // Nothing configured -> no tags at all; the fold line is the sheet's only new element.
    expect(html).not.toContain('<span class="tag">');
    // The inline error line starts hidden.
    expect(html).toContain('<div class="hint" id="c-err" hidden></div>');
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
    expect(html).not.toMatch(/<details[^>]*\sopen[\s>]/);
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
    const html = openSheet({
      id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key",
      proxy: "socks5://127.0.0.1:7890", proxyUsername: "pxuser", proxyPassword: MASK, jump: "c1",
    });
    expect(html).toContain('id="c-proxy" value="socks5://127.0.0.1:7890" placeholder="socks5://127.0.0.1:7890"');
    expect(html).toContain('id="c-proxy-user" value="pxuser"');
    // The sentinel rides into the input like the MCP sheet's password: masked, and sent
    // back untouched so the server keeps the stored secret.
    expect(html).toContain('id="c-proxy-pass" type="password" value="' + MASK + '"');
    // The dropdown: None first, every other connection offered, self excluded, jump picked.
    expect(html).toContain('<select id="c-jump"><option value="">None</option>');
    expect(html).toContain('<option value="c1" selected>bastion</option>');
    expect(html).not.toContain('value="c2"');
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
    const get = (id: string) => (doc as unknown as { getElementById(k: string): FakeNode }).getElementById(id);
    const err = get("c-err");
    expect(err.hidden).toBe(false);
    expect(err.textContent).toContain("jump cycle detected");
    expect(err.style.color).toBe("var(--red)");
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
    const plain = mods.polling.connRowHtml({ id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key", state: "idle" });
    expect(plain).not.toContain('<span class="tag">');

    const proxied = mods.polling.connRowHtml({ id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key", state: "idle", proxy: "socks5://127.0.0.1:7890" });
    expect(proxied).toContain('<span class="tag">proxy</span>');
    expect(proxied).not.toContain('<span class="tag">via');

    const jumping = mods.polling.connRowHtml({ id: "c2", name: "db", host: "10.0.0.9", port: 22, username: "u", authType: "key", state: "idle", jump: "c1" });
    expect(jumping).toContain('<span class="tag">via bastion</span>');
  });
});
