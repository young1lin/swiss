import { describe, it, expect, beforeAll, beforeEach, vi } from "vitest";

/* The zai-vision type (the @z_ai/mcp-server port compiled in): the form's contract and the
   metered-API guard. What this suite pins on the browser side:
     - the field set carries apiKey/mode/model and no url or auth box (the type decides
     - readFields round-trips the ${...} key reference untouched - the panel never expands it
     - no Test button path: runConnTest refuses a metered call without firing a request
   Same micro-DOM harness as admin-oauth.test.ts (no DOM library in this repo). */

class FakeNode {
  tag: string;
  attrs: Record<string, string> = {};
  dataset: Record<string, string> = {};
  className = "";
  textContent = "";
  innerHTML = "";
  id = "";
  type = "button";
  checked = false;
  value = "";
  hidden = false;
  disabled = false;
  style: Record<string, string> = {};
  onclick: ((ev?: unknown) => void) | null = null;
  constructor(tag: string) {
    this.tag = tag.toUpperCase();
  }
  setAttribute(k: string, v: string) {
    this.attrs[k] = v;
    if (k === "id") this.id = v;
  }
  getAttribute(k: string) {
    return Object.prototype.hasOwnProperty.call(this.attrs, k) ? this.attrs[k] : null;
  }
  querySelector() { return null; }
  querySelectorAll() { return []; }
  contains() { return false; }
  appendChild(n: FakeNode) { return n; }
  removeChild(n: FakeNode) { return n; }
  remove() {}
  addEventListener() {}
  select() {}
}

const byId = new Map<string, FakeNode>();
const doc = {
  getElementById(id: string) {
    if (!byId.has(id)) byId.set(id, new FakeNode("div"));
    return byId.get(id)!;
  },
  createElement: (t: string) => new FakeNode(t),
  querySelector: () => null,
  querySelectorAll: () => [] as FakeNode[],
  body: new FakeNode("body"),
  addEventListener() {},
  activeElement: null as FakeNode | null,
};

let fields: typeof import("../../src/admin_assets/js/fields.js");
let detail: typeof import("../../src/admin_assets/js/detail.js");
let util: typeof import("../../src/admin_assets/js/util.js");

beforeAll(async () => {
  (globalThis as unknown as Record<string, unknown>).document = doc;
  (globalThis as unknown as Record<string, unknown>).window = { open: vi.fn(), addEventListener() {} };
  fields = await import("../../src/admin_assets/js/fields.js");
  util = await import("../../src/admin_assets/js/util.js");
  detail = await import("../../src/admin_assets/js/detail.js");
});

beforeEach(() => {
  byId.clear();
  util.state.detail = null;
});

function fakeDetail(name: string, config: Record<string, unknown> | null) {
  const kd = { items: [], nextCursor: undefined, total: undefined, loaded: false, loading: false, error: null, cursors: [""], pageSize: 50 };
  return {
    name, tab: "tools", config, source: "managed", editing: true, editType: "zai-vision", editVals: null,
    oauth: undefined, oauthBusy: false,
    tools: { ...kd }, resources: { ...kd }, prompts: { ...kd },
    run: { tool: null, result: null, running: false, hist: null, histTool: null, histLoading: false, histOpen: false, histSelSeq: null, histFull: {}, histQ: "" },
    calls: null, stderr: "", callsOpen: {}, callsLoading: false, callsPage: 0, callsMore: false, callsFull: {},
  } as never;
}

describe("the zai-vision type: the native GLM vision tools", () => {
  it("carries apiKey/mode/model and nothing that would pick an endpoint twice", () => {
    const keys = fields.TYPE_FIELDS["zai-vision"].map((f: { k: string }) => f.k);
    expect(keys).toContain("description");
    expect(keys).toContain("apiKey");
    expect(keys).toContain("mode");
    expect(keys).toContain("model");
    expect(keys).toContain("baseUrl");
    // No url (that is mode/baseUrl's job), no auth box (no OAuth on this API).
    expect(keys).not.toContain("url");
    expect(keys).not.toContain("auth");
    expect(fields.TYPE_LABELS["zai-vision"]).toContain("zai-vision");
  });

  it("readFields keeps the ${...} key reference exactly as typed", () => {
    byId.set("a-name", Object.assign(new FakeNode("input"), { value: "zai" }));
    byId.set("a-apiKey", Object.assign(new FakeNode("input"), { value: "${Z_AI_API_KEY}" }));
    byId.set("a-mode", Object.assign(new FakeNode("input"), { value: "ZHIPU" }));
    const read = fields.readFields("zai-vision", "a-");
    expect(read.apiKey).toBe("${" + "Z_AI_API_KEY}");
    expect(read.mode).toBe("ZHIPU");
  });

  it("is not testable: every call would spend tokens", () => {
    expect(fields.TESTABLE_TYPES).not.toContain("zai-vision");
  });

  it("runConnTest answers the honest message without firing a request", async () => {
    util.state.detail = fakeDetail("zai", { type: "zai-vision", apiKey: "${" + "Z_AI_API_KEY}" });
    let fired = 0;
    (globalThis as unknown as Record<string, unknown>).fetch = async () => { fired++; return { ok: true, status: 200, json: async () => ({}) } as never; };
    await detail.runConnTest("e-");
    expect(fired).toBe(0);
    expect(doc.getElementById("e-test-out").textContent).toContain("spends tokens");
  });
});