import { beforeAll, describe, expect, it, vi } from "vitest";

/* docs/33 — the Logs JSON viewer. C1: the highlighter is a pure function over the pretty
   text; the copy affordance is markup plus a clipboard helper. Both are tested here as
   units and through logsBody markup, the way admin-logs-search pins markup. */

vi.mock("../../src/admin_assets/js/util.js", () => {
  const toasts: string[] = [];
  return {
    esc: (s: string) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;"),
    icon: () => "",
    state: { detail: null },
    toast: (msg: string) => { toasts.push(msg); },
    __toasts: toasts,
  };
});
vi.mock("../../src/admin_assets/js/sidebar.js", () => ({ rowOf: () => null }));

let logs: typeof import("../../src/admin_assets/js/logs.js");
let util: typeof import("../../src/admin_assets/js/util.js") & { __toasts: string[] };

beforeAll(async () => {
  logs = await import("../../src/admin_assets/js/logs.js");
  util = (await import("../../src/admin_assets/js/util.js")) as never;
});

function call(over: Record<string, unknown> = {}) {
  return { seq: 7, at: "2026-09-17T10:00:00.000Z", via: "panel", client: "panel", ms: 5, chars: 100, ok: true, tool: "redis_command", args: '{"command":"GET","args":["k"]}', output: '{"v":1}', preview: false, ...over } as never;
}
function stub(over: Record<string, unknown> = {}) {
  return {
    name: "redis", calls: [], callsPage: 0, callsMore: false,
    callsFull: {}, callsOpen: {}, stderr: "", callsQ: "",
    ...over,
  } as never;
}

describe("docs/33 C1: hlJson colours the pretty text, never trusts it", () => {
  it("keys, strings, numbers and booleans get their token spans", () => {
    const html = logs.hlJson('{\n  "cmd": "GET",\n  "n": -1.5,\n  "on": true,\n  "x": null\n}');
    expect(html).toContain('<span class="k">&quot;cmd&quot;</span>');
    expect(html).toContain('<span class="s">&quot;GET&quot;</span>');
    expect(html).toContain('<span class="n">-1.5</span>');
    expect(html).toContain('<span class="b">true</span>');
    expect(html).toContain('<span class="b">null</span>');
  });

  it("escapes token text — a payload can never inject markup", () => {
    const html = logs.hlJson('{\n  "k": "<script>&x"\n}');
    expect(html).toContain("&lt;script&gt;");
    expect(html).toContain("&amp;");
    expect(html).not.toContain("<script>");
  });

  it("text that is not pretty JSON is returned untouched, unspanned", () => {
    const plain = "upstream said no (5xx)";
    expect(logs.hlJson(plain)).toBe(plain);
  });

  it("the arguments and result pres carry the highlight class with highlighted content", () => {
    const html = logs.logsBody(stub({ calls: [call()], callsOpen: { 7: true } }));
    expect(html).toMatch(/<pre class="logs jhl"[^>]*>/);
    expect(html).toContain('<span class="k">&quot;command&quot;</span>');
  });
});

describe("docs/33 C1: every block offers copy", () => {
  it("both label rows carry an icon copy button addressing its block", () => {
    const html = logs.logsBody(stub({ calls: [call()], callsOpen: { 7: true } }));
    expect(html).toContain('aria-label="Copy arguments"');
    expect(html).toContain('aria-label="Copy result"');
    expect(html).toMatch(/data-copy="args:7"/);
    expect(html).toMatch(/data-copy="out:7"/);
  });

  it("copyLogText uses the async clipboard and says Copied", async () => {
    const wrote: string[] = [];
    vi.stubGlobal("navigator", {
      clipboard: { writeText: (t: string) => { wrote.push(t); return Promise.resolve(); } },
    });
    await logs.copyLogText("{\"a\":1}");
    expect(wrote).toEqual(['{"a":1}']);
    expect(util.__toasts).toContain("Copied");
  });

  it("falls back to the legacy path when the clipboard API withholds", async () => {
    vi.stubGlobal("navigator", {}); // a browser that withholds the async clipboard entirely
    let execed = 0;
    const ta = { value: "", style: {}, setAttribute() {}, select() {} } as never;
    vi.stubGlobal("document", {
      createElement: () => ta,
      body: { appendChild() {}, removeChild() {} },
      execCommand: () => { execed++; return true; },
    });
    await logs.copyLogText("x");
    expect(execed).toBe(1);
    expect(util.__toasts[util.__toasts.length - 1]).toBe("Copied");
    vi.unstubAllGlobals();
  });
});
