import { beforeAll, describe, expect, it, vi } from "vitest";

/* docs/33 — the Logs JSON viewer. Compact wire text is parsed into the restrained tree;
   non-JSON stays plain text. Copy is tested both per block and per node. */

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
/** The exact row whose key span is `raw`, regardless of what other rows contain. */
function rowByKey(node: Record<string, unknown>, raw: string): Record<string, unknown> | undefined {
  let hit: Record<string, unknown> | undefined;
  (function walk(n: Record<string, unknown>) {
    (n.children as Record<string, unknown>[] || []).forEach((c) => {
      if (!hit && String(c.className || "").includes("jt-row") && keyOf(c) === raw) hit = c;
      walk(c);
    });
  })(node);
  return hit;
}

describe("docs/33: compact wire text is formatted only for display", () => {
  it("pretty-prints compact JSON while preserving the gateway truncation note", () => {
    const raw = '{"rowCount":1,"rows":[{"id":1}]}\n\n[showing the first 1 of 4 items. Narrow the request.]';
    expect(logs.fmtJson(raw)).toBe(
      '{\n  "rowCount": 1,\n  "rows": [\n    {\n      "id": 1\n    }\n  ]\n}' +
      '\n\n[showing the first 1 of 4 items. Narrow the request.]',
    );
  });

  it("leaves non-JSON and truncated JSON unchanged", () => {
    expect(logs.fmtJson("upstream failed")).toBe("upstream failed");
    expect(logs.fmtJson('{"rows":[')).toBe('{"rows":[');
  });
});

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

  it("shows the useful top level directly and keeps nested containers folded", () => {
    vi.stubGlobal("document", { createElement: el } as never);
    const host = el("div");
    const open: Record<string, boolean> = {};
    logs.buildJsonTree(host, VALUE, open);
    expect(rowByKey(host, "command"), "a top-level leaf row exists").toBeTruthy();
    const argsRow = rowByKey(host, "args");
    expect(argsRow, "the top-level array row exists").toBeTruthy();
    expect(String(argsRow!.textContent)).toContain("[2]");
    expect(rowByKey(host, "nested"), "the top-level object row exists").toBeTruthy();
    expect(rowByKey(host, "deep"), "nested content starts folded").toBeUndefined();
    expect(open["/args/"], "top-level containers record their folded default").toBe(false);
    const hasSyntheticRoot = (host.children as Record<string, unknown>[]).some((c) => {
      const first = (c.children as Record<string, unknown>[] || [])[0];
      return String(c.className || "").includes("jt-node") && first && keyOf(first) === "";
    });
    expect(hasSyntheticRoot, "there is no redundant synthetic root row").toBe(false);
    vi.unstubAllGlobals();
  });

  it("renders empty object and array roots honestly", () => {
    vi.stubGlobal("document", { createElement: el } as never);
    const objectHost = el("div");
    const arrayHost = el("div");
    logs.buildJsonTree(objectHost, {}, {});
    logs.buildJsonTree(arrayHost, [], {});
    expect(String(objectHost.textContent)).toBe("{}");
    expect(String(arrayHost.textContent)).toBe("[]");
    vi.unstubAllGlobals();
  });

  it("children are lazy: a folded top-level container builds nothing until opened", () => {
    vi.stubGlobal("document", { createElement: el } as never);
    const host = el("div");
    const open: Record<string, boolean> = {};
    logs.buildJsonTree(host, { rows: [{ id: 1 }] }, open);
    const rows = rowByKey(host, "rows")!;
    const wrap = (host.children as Record<string, unknown>[]).find((c) =>
      String(c.className || "").includes("jt-node") && (c.children as Record<string, unknown>[])[0] === rows)!;
    const kids = (wrap.children as Record<string, unknown>[])[1];
    expect(String(rows.textContent)).toContain("[1]");
    expect((kids.children as unknown[]).length, "nothing is built under the folded result array").toBe(0);
    vi.unstubAllGlobals();
  });

  it("a chevron opens a top-level node, records its path, and a rebuild restores it", () => {
    vi.stubGlobal("document", { createElement: el } as never);
    const open: Record<string, boolean> = {};
    const host1 = el("div");
    logs.buildJsonTree(host1, { a: { b: 1 } }, open);
    const aRow = rowByKey(host1, "a")!;
    expect(String(aRow.textContent)).toContain("{1}");
    const chev = (aRow.children as Record<string, unknown>[]).find((c) => c.tag === "button" && !((c as { dataset: Record<string, string> }).dataset.copy));
    ((chev as { onclick: () => void }).onclick)();
    expect(open["/a/"], "the expansion is recorded under a stable path").toBe(true);
    expect((chev as { attrs: Record<string, string> }).attrs["aria-expanded"]).toBe("true");
    const host2 = el("div");
    logs.buildJsonTree(host2, { a: { b: 1 } }, open);
    expect(rowByKey(host2, "b"), "a rebuild with the same open state restores its children").toBeTruthy();
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

  it("a dataset-string seq (\"4\" from data-callseq) still mounts — found live on 19998", () => {
    const rows: Record<string, unknown>[] = [];
    const rowEl = {
      getAttribute: (k: string) => (k === "data-seq" ? "4" : null),
      querySelector: () => null, // no slot, no pre — nothing to mount, but the row must not be skipped
      children: [], className: "call open", dataset: {}, style: {},
    } as never;
    vi.stubGlobal("document", {
      querySelectorAll: () => [rowEl],
      createElement: el,
    } as never);
    const d = { calls: [{ seq: 4, args: "plain", output: "plain" }], callsFull: {} } as never;
    logs.mountJsonTrees(d, "4"); // a string, exactly as wireTabBody delivers it
    // "not skipped" is observable only when a mountable value exists: use a parseable one.
    const rowEl2 = {
      getAttribute: (k: string) => (k === "data-seq" ? "4" : null),
      querySelector: (sel: string) => (String(sel).includes("args:4") ? host4 : null),
      children: [], className: "call open", dataset: {}, style: {},
    } as never;
    const host4 = el("div");
    (rowEl2 as { querySelector: (s: string) => unknown }).querySelector = (sel: string) =>
      String(sel).includes("args:4") ? host4 : null;
    vi.stubGlobal("document", {
      querySelectorAll: () => [rowEl2],
      createElement: el,
    } as never);
    const d2 = { calls: [{ seq: 4, args: '{"k":1}', output: "plain" }], callsFull: {} } as never;
    logs.mountJsonTrees(d2, "4");
    expect(String(host4.className)).toContain("jtree-in");
    expect((host4.children as unknown[]).length, "the tree built for the string-addressed row").toBeGreaterThan(0);
    vi.unstubAllGlobals();
    void rows;
  });

  it("docs/33 readability: every key carries a JSON colon span", () => {
    vi.stubGlobal("document", { createElement: el } as never);
    const host = el("div");
    logs.buildJsonTree(host, { cursor: "5088", keys: [], done: false }, {});
    const cursorRow = rowByKey(host, "cursor")!;
    const cols = (cursorRow.children as Record<string, unknown>[]).filter((c) => String(c.className).includes("jt-col"));
    expect(cols.length, "one colon span after the key").toBe(1);
    expect(String((cols[0] as { textContent: string }).textContent)).toBe(":");
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
