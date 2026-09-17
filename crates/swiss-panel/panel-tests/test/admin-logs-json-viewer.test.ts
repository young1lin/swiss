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

describe("docs/33 C1+C2: the block body division — tree when it parses, pre when it does not", () => {
  it("a parseable arguments/result block renders a tree slot, not a pre", () => {
    const html = logs.logsBody(stub({ calls: [call()], callsOpen: { 7: true } }));
    expect(html).toContain('data-jtree="args:7"');
    expect(html).toContain('data-jtree="out:7"');
    expect(html).not.toMatch(/<pre class="logs/);
  });

  it("a non-JSON result keeps a plain escaped pre, an error keeps the red one", () => {
    const html = logs.logsBody(stub({
      calls: [call({ args: "GET k", output: "upstream said <no>", ok: false })],
      callsOpen: { 7: true },
    }));
    expect(html).toMatch(/<pre class="logs">GET k<\/pre>/);
    expect(html).toMatch(/<pre class="logs err"[^>]*>upstream said &lt;no&gt;/);
  });
});

/* docs/33 C2 — the tree builder is a pure DOM function; the discovery glue (querySelectorAll
   over the painted tabbody) is one line and is proven live on 19998 per the panel rules. */
function el(tag: string): Record<string, unknown> {
  return {
    tag, children: [] as unknown[], className: "", title: "", innerHTML: "",
    dataset: {} as Record<string, string>, style: {} as Record<string, string>,
    attrs: {} as Record<string, string>, removed: false, _text: "",
    set textContent(v: string) { this._text = String(v); },
    get textContent() {
      return this._text + (this.children as { textContent: string }[]).map((c) => c.textContent).join("");
    },
    setAttribute(k: string, v: string) { this.attrs[k] = v; },
    getAttribute(k: string) { return this.attrs[k] ?? null; },
    appendChild(n: Record<string, unknown>) { this.children.push(n); return n; },
    removeChild(n: Record<string, unknown>) { this.children = (this.children as unknown[]).filter((c) => c !== n); return n; },
    get firstChild() { return (this.children as unknown[])[0] || null; },
    remove() { this.removed = true; },
  };
}
function rowsOf(node: Record<string, unknown>): Record<string, unknown>[] {
  return (node.children as Record<string, unknown>[]).filter((c) => c.tag === "div");
}
function findRow(node: Record<string, unknown>, key: string): Record<string, unknown> | undefined {
  const all: Record<string, unknown>[] = [];
  (function walk(n: Record<string, unknown>) {
    (n.children as Record<string, unknown>[] || []).forEach((c) => {
      if (String(c.className || "").includes("jt-row") && String(c.textContent).includes(key)) all.push(c);
      walk(c);
    });
  })(node);
  return all[0];
}
function keyOf(row: Record<string, unknown>): string {
  const k = (row.children as Record<string, unknown>[]).find((c) => String(c.className).includes("jt-key"));
  return k ? String(k.textContent) : "";
}
/** The exact row whose key span IS `raw` (quoted key text), regardless of what other rows contain. */
function rowByKey(node: Record<string, unknown>, raw: string): Record<string, unknown> | undefined {
  let hit: Record<string, unknown> | undefined;
  (function walk(n: Record<string, unknown>) {
    (n.children as Record<string, unknown>[] || []).forEach((c) => {
      if (!hit && String(c.className || "").includes("jt-row") && keyOf(c) === JSON.stringify(raw)) hit = c;
      walk(c);
    });
  })(node);
  return hit;
}

describe("docs/33 C2: parseJsonBlock only accepts objects and arrays", () => {
  it("parses an object or an array, rejects everything else", () => {
    expect(logs.parseJsonBlock('{"a":1}')).toEqual({ a: 1 });
    expect(logs.parseJsonBlock("[1,2]")).toEqual([1, 2]);
    expect(logs.parseJsonBlock("plain text")).toBeNull();
    expect(logs.parseJsonBlock("42")).toBeNull();
    expect(logs.parseJsonBlock("{broken")).toBeNull();
    expect(logs.parseJsonBlock("")).toBeNull();
  });
});

describe("docs/33 C2: buildJsonTree", () => {
  const VALUE = { command: "GET", args: ["k1", "k2"], nested: { deep: { x: 1 } } };

  it("renders rows with keys, chevrons on containers, and default-opens the first two depths", () => {
    vi.stubGlobal("document", { createElement: el } as never);
    const host = el("div");
    const open: Record<string, boolean> = {};
    logs.buildJsonTree(host, VALUE, open);
    expect(findRow(host, "command"), "a leaf row exists").toBeTruthy();
    const argsRow = findRow(host, "args");
    expect(argsRow, "the array container row exists").toBeTruthy();
    expect(String(argsRow!.textContent)).toContain("[ 2 items ]");
    expect(findRow(host, "nested"), "depth-1 container row renders").toBeTruthy();
    expect(findRow(host, "deep"), "depth-1 is open by default: its child row is visible").toBeTruthy();
    expect(findRow(host, "x"), "depth-2 containers start closed").toBeUndefined();
    vi.unstubAllGlobals();
  });

  it("children are lazy: a closed container has no built kids until its chevron opens it", () => {
    vi.stubGlobal("document", { createElement: el } as never);
    const host = el("div");
    const open: Record<string, boolean> = {};
    // depth rule: depth-1 containers open, depth-2 closed — lvl2's kids must not exist yet.
    logs.buildJsonTree(host, { lvl1: { lvl2: { leaf: 1 } } }, open);
    const lvl2 = rowByKey(host, "lvl2");
    expect(lvl2, "the depth-2 container row renders").toBeTruthy();
    expect(String(lvl2!.textContent)).toContain("{ 1 key }");
    // lvl2's kids container is the sibling of its row, both under the same jt-node wrap
    let lvl2Kids: Record<string, unknown> | undefined;
    (function walk(n: Record<string, unknown>) {
      (n.children as Record<string, unknown>[] || []).forEach((c) => {
        const first = (c.children as Record<string, unknown>[] || [])[0];
        if (String(c.className || "").includes("jt-node") && first && keyOf(first) === '"lvl2"') {
          lvl2Kids = (c.children as Record<string, unknown>[])[1];
        }
        walk(c);
      });
    })(host);
    expect(lvl2Kids && String(lvl2Kids!.className).includes("jt-kids"), "lvl2's kids container exists").toBe(true);
    expect((lvl2Kids!.children as unknown[]).length, "nothing built under a closed container").toBe(0);
    vi.unstubAllGlobals();
  });

  it("a chevron click opens the node, records the path, and a rebuild restores it", () => {
    vi.stubGlobal("document", { createElement: el } as never);
    const open: Record<string, boolean> = {};
    const host1 = el("div");
    // b sits at depth 2: closed by default, so its chevron is the first honest toggle.
    logs.buildJsonTree(host1, { a: { b: { c: 1 } } }, open);
    const bRow = rowByKey(host1, "b");
    expect(String(bRow!.textContent)).toContain("{ 1 key }");
    const chev = (bRow!.children as Record<string, unknown>[]).find((c) => c.tag === "button" && !((c as { dataset: Record<string, string> }).dataset.copy));
    ((chev as { onclick: () => void }).onclick)();
    expect(open["/a/b/"], "the expansion is recorded under a stable path").toBe(true);
    const host2 = el("div");
    logs.buildJsonTree(host2, { a: { b: { c: 1 } } }, open);
    expect(rowByKey(host2, "c"), "a rebuild with the same open state shows the node open").toBeTruthy();
    vi.unstubAllGlobals();
  });

  it("per-node copy: containers copy pretty JSON, leaf strings copy raw text", async () => {
    const wrote: string[] = [];
    vi.stubGlobal("navigator", { clipboard: { writeText: (t: string) => { wrote.push(t); return Promise.resolve(); } } });
    vi.stubGlobal("document", { createElement: el } as never);
    const host = el("div");
    logs.buildJsonTree(host, { nested: { a: 1 }, s: "long value" }, {});
    const copyBtns = (row: Record<string, unknown>) =>
      (row.children as Record<string, unknown>[]).filter((c) => c.tag === "button" && String((c as { dataset: Record<string, string> }).dataset.copy) === "1");
    const nestedRow = rowByKey(host, "nested")!;
    const sRow = rowByKey(host, "s")!;
    await ((copyBtns(nestedRow)[0] as { onclick: () => Promise<void> }).onclick)();
    await ((copyBtns(sRow)[0] as { onclick: () => Promise<void> }).onclick)();
    expect(wrote[0]).toBe('{\n  "a": 1\n}');
    expect(wrote[1]).toBe("long value");
    vi.unstubAllGlobals();
  });

  it("a long leaf string truncates for display at 200 characters", () => {
    vi.stubGlobal("document", { createElement: el } as never);
    const host = el("div");
    const long = new Array(240).join("x"); // 239 chars — over the 200 display cap
    logs.buildJsonTree(host, { s: long }, {});
    const row = findRow(host, "s")!;
    const text = String(row.textContent);
    expect(text.length).toBeLessThan(220);
    expect(text).toContain("…");
    vi.unstubAllGlobals();
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
