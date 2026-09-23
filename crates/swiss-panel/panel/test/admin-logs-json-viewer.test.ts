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

import { beforeAll, describe, expect, it, vi } from "vitest";

// @vitest-environment happy-dom

/* docs/33 C3 — the Logs JSON view. A call's arguments and reply are one formatted code block,
   the same for every MCP: standard indented JSON, a string that holds JSON shown as that JSON
   (behind a "decoded" marker), JSON followed by prose split into code + text, anything else as
   it arrived. Copy hands over valid JSON of what is shown; Copy raw the stored text. */

vi.mock("../src/util.js", () => {
  const toasts: string[] = [];
  return {
    esc: (s: string) => String(s).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;"),
    icon: () => "",
    iconNode: () => document.createElement("span"),
    state: { detail: null },
    toast: (msg: string) => { toasts.push(msg); },
    __toasts: toasts,
  };
});
vi.mock("../src/sidebar.js", () => ({ rowOf: () => null }));

let logs: typeof import("../src/logs.js");
let jv: typeof import("../src/ui/json-view.js");
let util: typeof import("../src/util.js") & { __toasts: string[] };

beforeAll(async () => {
  logs = await import("../src/logs.js");
  jv = await import("../src/ui/json-view.js");
  util = (await import("../src/util.js")) as never;
});

function call(over: Record<string, unknown> = {}) {
  return { seq: 7, at: "2026-09-17T10:00:00.000Z", via: "panel", client: "panel", ms: 5, chars: 100, ok: true, tool: "redis_command", args: '{"command":"GET","args":["k"]}', output: '{"v":1}', preview: false, ...over } as never;
}
function stub(over: Record<string, unknown> = {}) {
  return {
    name: "redis", calls: [], callsPage: 0, callsMore: false,
    callsFull: {}, callsOpen: {}, callsGone: {}, callsAll: {}, stderr: "", callsQ: "",
    ...over,
  } as never;
}

/* docs/37 R5: logsBodyNode paints a real tree — the assertions read the painted DOM. */
function paint(over: Record<string, unknown> = {}): HTMLElement {
  const host = document.createElement("div");
  host.append(...[logs.logsBodyNode(stub(over) as never)].flat().filter((n): n is Node => n != null));
  return host;
}
function block(host: HTMLElement, key: string): HTMLElement {
  return host.querySelector<HTMLElement>('[data-blk="' + key + '"]')!;
}

/* The shapes the operator's MCPs actually return, written as synthetic samples. */
const ZHIPU = JSON.stringify(JSON.stringify([{ title: "Rust 1.90", link: "https://example.test/a", content: "line one\nline two" }]));
const REDIS_HASH = JSON.stringify({ profile: JSON.stringify(JSON.stringify({ id: 1, tags: ["a"] })), n: "50", flag: "true" });
const FIGMA = '[{"type":"image","mime":"image/png"}]\n\nThe screenshot above is the selected frame.';

describe("docs/33 C3: splitJsonBlock — what counts as JSON", () => {
  it("a whole JSON value is one block with no tail", () => {
    expect(jv.splitJsonBlock('{"a":1}')).toEqual({ value: { a: 1 }, tail: "" });
    expect(jv.splitJsonBlock('  [1,2]\n')).toEqual({ value: [1, 2], tail: "" });
    expect(jv.splitJsonBlock("50"), "a reply that is a bare number is still JSON").toEqual({ value: 50, tail: "" });
  });

  it("JSON followed by prose keeps the JSON and hands back the prose", () => {
    expect(jv.splitJsonBlock(FIGMA)).toEqual({ value: [{ type: "image", mime: "image/png" }], tail: "The screenshot above is the selected frame." });
    const noted = jv.splitJsonBlock('{"rows":[{"id":1}]}\n\n[showing the first 1 of 4 items. Narrow the request.]');
    expect(noted!.tail).toBe("[showing the first 1 of 4 items. Narrow the request.]");
  });

  it("a brace inside a string closes nothing", () => {
    expect(jv.splitJsonBlock('{"q":"a } b"} tail')).toEqual({ value: { q: "a } b" }, tail: "tail" });
  });

  it("text, markdown, a clipped preview and empty text are not JSON", () => {
    expect(jv.splitJsonBlock("upstream failed")).toBeNull();
    expect(jv.splitJsonBlock("# Title\n\nbody")).toBeNull();
    expect(jv.splitJsonBlock('{"rows":[')).toBeNull();
    expect(jv.splitJsonBlock("  ")).toBeNull();
  });
});

describe("docs/33 C3: decodeStrings — a string that holds JSON is shown as JSON", () => {
  it("a search reply that is one JSON string is decoded once", () => {
    const v = jv.decodeStrings(jv.splitJsonBlock(ZHIPU)!.value);
    expect(v).toBeInstanceOf(jv.DecodedString);
    expect((v as InstanceType<typeof jv.DecodedString>).layers).toBe(1);
    expect(jv.plainValue(v)).toEqual([{ title: "Rust 1.90", link: "https://example.test/a", content: "line one\nline two" }]);
  });

  it("a redis value encoded twice is peeled twice; \"50\" and \"true\" stay strings", () => {
    const v = jv.decodeStrings(JSON.parse(REDIS_HASH)) as Record<string, unknown>;
    expect((v.profile as InstanceType<typeof jv.DecodedString>).layers).toBe(2);
    expect(jv.plainValue(v.profile)).toEqual({ id: 1, tags: ["a"] });
    expect(v.n).toBe("50");
    expect(v.flag).toBe("true");
  });

  it("hasDecoded answers whether anything was decoded", () => {
    expect(jv.hasDecoded(jv.decodeStrings({ a: "x", b: [1, "{bad"] }))).toBe(false);
    expect(jv.hasDecoded(jv.decodeStrings({ a: ['{"b":1}'] }))).toBe(true);
  });
});

describe("docs/33 C3: Copy is valid JSON of what is shown", () => {
  it("a decoded reply copies as the structure it holds", () => {
    const text = jv.formattedCopyText(ZHIPU);
    expect(JSON.parse(text)).toEqual([{ title: "Rust 1.90", link: "https://example.test/a", content: "line one\nline two" }]);
    expect(text).toContain('\n  {\n    "title": "Rust 1.90"');
  });

  it("the prose after the JSON rides along below it; text that is not JSON is unchanged", () => {
    const text = jv.formattedCopyText(FIGMA);
    const [head, tail] = text.split("\n\n");
    expect(JSON.parse(head)).toEqual([{ type: "image", mime: "image/png" }]);
    expect(tail).toBe("The screenshot above is the selected frame.");
    expect(jv.formattedCopyText("upstream failed")).toBe("upstream failed");
  });
});

describe("docs/33 C3: jsonCodeNode — one formatted code block", () => {
  it("prints exactly JSON.stringify(v, null, 2) for a plain value", () => {
    const v = { s: "a \"q\" \\ b", n: -1.5e3, t: true, f: false, z: null, e: {}, a: [], nest: [{ k: [1, { deep: "ü" }] }] };
    const code = jv.jsonCodeNode(v, false);
    expect(code.node.tagName).toBe("PRE");
    expect(code.node.className).toBe("jv");
    expect(code.node.textContent).toBe(JSON.stringify(v, null, 2));
    expect(code.lines).toBe(JSON.stringify(v, null, 2).split("\n").length);
  });

  it("colours by token: key, string, number, literal", () => {
    const pre = jv.jsonCodeNode({ k: "v", n: 2, b: true }, false).node;
    expect(pre.querySelector(".jv-k")!.textContent).toBe('"k"');
    expect(pre.querySelector(".jv-s")!.textContent).toBe('"v"');
    expect(pre.querySelector(".jv-n")!.textContent).toBe("2");
    expect(pre.querySelector(".jv-l")!.textContent).toBe("true");
  });

  it("a string's line breaks are real breaks; everything else stays escaped", () => {
    const pre = jv.jsonCodeNode({ sql: "SELECT 1\r\nFROM t", path: "C:\\new", tab: "a\tb" }, false).node;
    const text = pre.textContent!;
    expect(text).toContain('"SELECT 1\nFROM t"');
    expect(text, "no escape and no stray CR where the break was").not.toMatch(/SELECT 1(\\r|\\n|\r)/);
    expect(text).toContain('"C:\\\\new"');
    expect(text).toContain('"a\\tb"');
  });

  it("a decoded string carries a marker whose words are not selectable text", () => {
    const one = jv.jsonCodeNode(jv.decodeStrings(JSON.parse(ZHIPU)), false).node;
    const mark = one.querySelector(".jv-dec")!;
    expect(mark.getAttribute("data-label")).toBe("decoded");
    expect(mark.textContent, "the label lives in ::before, so a drag-copy never picks it up").toBe("");
    expect(one.textContent!.startsWith('[\n  {\n    "title": "Rust 1.90"'), "a selection starts at the JSON, not at the marker").toBe(true);
    const two = jv.jsonCodeNode(jv.decodeStrings(JSON.parse(REDIS_HASH)), false).node;
    expect(two.querySelector(".jv-dec")!.getAttribute("data-label")).toBe("decoded ×2");
  });

  it("past 200 lines it stops building nodes but still counts every line; all shows everything", () => {
    const v = Array.from({ length: 300 }, (_, i) => i);
    const capped = jv.jsonCodeNode(v, false);
    expect(capped.lines).toBe(302);
    expect(capped.node.textContent!.split("\n").length).toBeLessThanOrEqual(jv.JV_LINES + 1);
    const whole = jv.jsonCodeNode(v, true);
    expect(whole.lines).toBe(302);
    expect(whole.node.textContent).toBe(JSON.stringify(v, null, 2));
  });
});

describe("docs/33 C3: a call paints its blocks as code, with Copy, Copy raw and Show all", () => {
  it("both blocks are formatted code blocks, each with its own two copies", () => {
    const host = paint({ calls: [call()], callsOpen: { 7: true } });
    expect(block(host, "args:7").querySelector("pre.jv")!.textContent).toBe('{\n  "command": "GET",\n  "args": [\n    "k"\n  ]\n}');
    expect(block(host, "out:7").querySelector("pre.jv")!.textContent).toBe('{\n  "v": 1\n}');
    expect(host.querySelector('[data-copy="args:7"]')!.getAttribute("aria-label")).toBe("Copy arguments");
    expect(host.querySelector('[data-copy="out:7"]')!.getAttribute("aria-label")).toBe("Copy result");
    expect(host.querySelector('[data-copyraw="args:7"]')!.textContent).toBe("Copy raw");
    expect(host.querySelector('[data-copyraw="out:7"]')).not.toBeNull();
    expect(host.querySelector(".jtree, [data-jtree]"), "the folding tree is gone").toBeNull();
  });

  it("a string-wrapped reply says it was decoded; JSON plus prose says so and keeps the prose", () => {
    const zhipu = paint({ calls: [call({ output: ZHIPU })] });
    expect(block(zhipu, "out:7").querySelector(".call-note")!.textContent).toBe("JSON decoded from a string");
    expect(block(zhipu, "out:7").querySelector(".jv-dec")).not.toBeNull();
    const figma = paint({ calls: [call({ output: FIGMA })] });
    expect(block(figma, "out:7").querySelector(".call-note")!.textContent).toBe("JSON + text");
    expect(block(figma, "out:7").querySelector("pre.jv-tail")!.textContent).toBe("The screenshot above is the selected frame.");
  });

  it("text stays as it arrived; an error stays red text even when it is JSON", () => {
    const host = paint({ calls: [call({ args: "GET k", output: '{"error":"no <such> key"}', ok: false })] });
    expect(block(host, "args:7").querySelector("pre.logs")!.textContent).toBe("GET k");
    expect(block(host, "args:7").querySelector("pre.jv")).toBeNull();
    expect(block(host, "out:7").querySelector("pre.logs.err")!.textContent).toBe('{"error":"no <such> key"}');
    const empty = paint({ calls: [call({ args: "", output: "" })] });
    expect(block(empty, "args:7").querySelector("pre.logs")!.textContent).toBe("(none)");
    expect(block(empty, "out:7").querySelector("pre.logs")!.textContent).toBe("(empty)");
  });

  it("the bodies are painted on a closed row too — opening is only a class flip", () => {
    const host = paint({ calls: [call()] });
    expect(host.querySelector(".call.open")).toBeNull();
    expect(host.querySelectorAll("pre.jv").length).toBe(2);
  });

  it("a long block ends in Show all N lines; once shown, the button is gone", () => {
    const big = JSON.stringify(Array.from({ length: 300 }, (_, i) => i));
    const capped = paint({ calls: [call({ output: big })] });
    const btn = capped.querySelector('[data-showall="out:7"]')!;
    expect(btn.textContent).toBe("Show all 302 lines");
    expect(capped.querySelector('[data-showall="args:7"]'), "a short block has no button").toBeNull();
    const whole = paint({ calls: [call({ output: big })], callsAll: { "out:7": true } });
    expect(whole.querySelector("[data-showall]")).toBeNull();
    expect(block(whole, "out:7").querySelector("pre.jv")!.textContent).toBe(JSON.stringify(JSON.parse(big), null, 2));
  });

  it("a clipped reply offers the fetch; a pruned one says so instead", () => {
    const clipped = call({ preview: true, chars: 5000, output: '{"rows":[' });
    const offered = paint({ calls: [clipped], callsOpen: { 7: true } });
    expect(offered.querySelector('[data-full="7"]'), "the button is there until the fetch lands").not.toBeNull();
    expect(block(offered, "out:7").querySelector("pre.logs")!.textContent, "a preview cut mid-value shows as text").toBe('{"rows":[');
    const pruned = paint({ calls: [clipped], callsOpen: { 7: true }, callsGone: { 7: true } });
    expect(pruned.querySelector('[data-full="7"]'), "nothing left to fetch — no button").toBeNull();
    expect(pruned.querySelector('[data-gone="7"]')!.textContent).toContain("no longer stored");
  });
});

describe("docs/33 C3: copy text and in-place repaint read the call row", () => {
  it("Copy gives formatted JSON of the decoded reply, Copy raw the stored text; the full reply wins", () => {
    const d = stub({ calls: [call({ output: ZHIPU, preview: true })] });
    expect(logs.callBlockCopyText(d, "out:7", true)).toBe(ZHIPU);
    expect(JSON.parse(logs.callBlockCopyText(d, "out:7", false)!)).toEqual(JSON.parse(JSON.parse(ZHIPU)));
    (d as { callsFull: Record<number, string> }).callsFull[7] = '{"whole":true}';
    expect(logs.callBlockCopyText(d, "out:7", true)).toBe('{"whole":true}');
    expect(logs.callBlockCopyText(d, "args:7", true)).toBe('{"command":"GET","args":["k"]}');
    expect(logs.callBlockCopyText(d, "out:8", false), "a row no longer on the page copies nothing").toBeNull();
  });

  it("repaintCallBlock swaps one block and leaves its sibling alone", () => {
    const d = stub({ calls: [call({ output: '{"rows":[', preview: true })] });
    const tb = document.createElement("div");
    tb.id = "tabbody";
    tb.append(logs.callNode(d, (d as { calls: never[] }).calls[0]));
    document.body.append(tb);
    const argsBefore = block(tb, "args:7");
    (d as { callsFull: Record<number, string> }).callsFull[7] = '{"rows":[1,2]}';
    logs.repaintCallBlock(d, "out:7");
    expect(block(tb, "out:7").querySelector("pre.jv")!.textContent).toBe('{\n  "rows": [\n    1,\n    2\n  ]\n}');
    expect(block(tb, "args:7"), "the arguments block is the same node").toBe(argsBefore);
    tb.remove();
  });
});

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

describe("docs/33 C1: the clipboard path", () => {
  it("copyLogText uses the async clipboard and says Copied", async () => {
    const wrote: string[] = [];
    vi.stubGlobal("navigator", {
      clipboard: { writeText: (t: string) => { wrote.push(t); return Promise.resolve(); } },
    });
    await logs.copyLogText("{\"a\":1}");
    expect(wrote).toEqual(['{"a":1}']);
    expect(util.__toasts).toContain("Copied");
    vi.unstubAllGlobals();
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
