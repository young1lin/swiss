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

import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Same DOM-stub technique as admin-data-cellview.test.ts: the panel ships browser ES modules,
// so the module graph needs the globals stubbed before it will evaluate under Node.
// The JSON body builds with h(), so nodes extend a Node stub (h() instanceof-checks children)
// and the cases serialize the built nodes with the escapes a browser's innerHTML would show -
// the assertions stay about INERT TEXT, not about innerHTML.
class NodeStub {}
(globalThis as unknown as { Node: unknown }).Node = NodeStub;

interface FakeNode {
  tag: string;
  className: string;
  open: boolean;
  textContent: string;
  children: FakeNode[];
  parentNode?: FakeNode;
  onclick?: () => void;
  appendChild(c: FakeNode): FakeNode;
}
const el = (tag = "div"): FakeNode & Record<string, unknown> => {
  const node: Record<string, unknown> = {
    tag, className: "", open: false, children: [],
    style: {}, dataset: {}, hidden: false, disabled: false, checked: false, value: "",
    textContent: "", innerHTML: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    setAttribute(k: string, v: string) { if (k === "class") node.className = v; },
    getAttribute: () => "", removeAttribute: () => {},
    addEventListener: () => {}, removeEventListener: () => {},
    appendChild(c: FakeNode) { (node.children as FakeNode[]).push(c); c.parentNode = node as unknown as FakeNode; return c; },
    removeChild: (c: unknown) => c,
    remove() {
      const p = node.parentNode as FakeNode | undefined;
      if (p) p.children = p.children.filter((c) => { return c !== (node as unknown); });
      node.parentNode = undefined;
    },
    replaceWith(x: FakeNode) {
      const p = node.parentNode as FakeNode | undefined;
      if (!p) return;
      p.children = p.children.map((c) => { return c === (node as unknown) ? x : c; });
      x.parentNode = p;
      node.parentNode = undefined;
    },
    querySelector: () => null, querySelectorAll: () => [],
    focus: () => {}, blur: () => {}, click: () => {},
    getBoundingClientRect: () => ({ left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }),
    contains: () => false, closest: () => null, insertAdjacentHTML: () => {},
  };
  node.cloneNode = () => el();
  Object.setPrototypeOf(node, NodeStub.prototype);
  return node as FakeNode & Record<string, unknown>;
};
/* Serialize the way innerHTML would read: text escaped, the class as an attribute. */
const escapeHtml = (s: string): string => {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
};
const serialize = (n: FakeNode): string => {
  if (n.tag === "#text") return escapeHtml(String(n.textContent));
  const cls = n.className ? ' class="' + n.className + '"' : "";
  const kids = (n.children || []).map((c) => { return serialize(c); }).join("");
  return "<" + n.tag + cls + ">" + kids + "</" + n.tag + ">";
};
const doc = {
  documentElement: el(), body: el(), head: el(),
  hidden: false, visibilityState: "visible", activeElement: null,
  getElementById: () => el(), createElement: (t: string) => el(t),
  createElementNS: (_ns: string, t: string) => el(t),
  createDocumentFragment: () => el("#document-fragment"),
  createTextNode: (s: string) => { const n = el("#text"); n.textContent = s; return n; },
  querySelector: () => null, querySelectorAll: () => [],
  addEventListener: () => {}, removeEventListener: () => {},
};
Object.assign(globalThis, {
  document: doc, window: globalThis,
  localStorage: { getItem: () => null, setItem: () => {}, removeItem: () => {} },
  location: { reload: () => {} },
  matchMedia: () => ({ matches: false, addEventListener: () => {}, addListener: () => {} }),
  confirm: () => true, alert: () => {},
  Blob: class {}, setInterval: () => 0, clearInterval: () => 0,
  addEventListener: () => {}, removeEventListener: () => {},
});

const value = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-value.ts")).href
) as {
  dbValueKind: (v: unknown, colType: string | null | undefined) => string;
  dbJsonValueNode: (v: unknown) => unknown;
  dbHexPreview: (v: string, maxBytes: number) => { text: string; bytes: number; truncated: boolean };
};
const html = (v: unknown): string => { return serialize(value.dbJsonValueNode(v) as FakeNode); };
/** Every node of a tree, depth first. */
const walk = (n: FakeNode): FakeNode[] => { return [n].concat(...(n.children || []).map(walk)); };

/* SPEC §data.grid — the value sheet. The presenter choice, the hex cap and the JSON tree are
   pure: every rule is value- and column-type-driven, never DOM-dependent, so the intents
   pin without a browser. The sheet itself is a thin wrapper over these. */
describe("dbValueKind — which of the four presentations a value opens as", () => {
  it("routes the \\x wire form to hex, whatever the column type says", () => {
    expect(value.dbValueKind("\\x00ff10", "blob")).toBe("hex");
    expect(value.dbValueKind("\\x00ff10", null)).toBe("hex"); // a query-result cell has no column type
    expect(value.dbValueKind("\\x", "bytea")).toBe("hex"); // empty binary rides the same form
  });

  it("routes a text-shaped binary column to text — the content IS the text", () => {
    // MySQL renders a UTF-8-valid BLOB as text on the wire (mysql.rs); the sheet shows the
    // text, not a hex dump of it.
    expect(value.dbValueKind("hello", "blob")).toBe("text");
    expect(value.dbValueKind("hello", "bytea")).toBe("text");
  });

  it("routes http(s) values to url and everything else non-JSON to text", () => {
    expect(value.dbValueKind("https://example.com/a?b=1", "text")).toBe("url");
    expect(value.dbValueKind("http://127.0.0.1:19999/x", null)).toBe("url");
    expect(value.dbValueKind("ftp://example.com/x", null)).toBe("text");
    expect(value.dbValueKind("plain words", "varchar")).toBe("text");
    expect(value.dbValueKind(42, "int")).toBe("text");
  });

  it("routes parseable JSON — a string that parses, or an object the adapter already parsed", () => {
    expect(value.dbValueKind('{"a":1}', "json")).toBe("json");
    expect(value.dbValueKind("[1,2]", "jsonb")).toBe("json");
    expect(value.dbValueKind({ a: 1 }, "jsonb")).toBe("json");
    expect(value.dbValueKind([1, 2], null)).toBe("json");
    // JSON-shaped but broken: the tree cannot be built — the sheet shows the text.
    expect(value.dbValueKind('{"a":', "json")).toBe("text");
  });

  it("keeps NULL out of the four kinds — the menu hides the item; the kind stays text", () => {
    expect(value.dbValueKind(null, "int")).toBe("text");
    expect(value.dbValueKind(undefined, "text")).toBe("text");
  });
});

describe("dbHexPreview — first N bytes, the real byte count, the truncated flag", () => {
  it("counts the underlying bytes, not the hex characters", () => {
    expect(value.dbHexPreview("\\x00ff10", 512)).toEqual({ text: "00 ff 10", bytes: 3, truncated: false });
  });

  it("groups sixteen bytes per line, one pair each", () => {
    var hex = Array(20).fill("ab").join("");
    var p = value.dbHexPreview("\\x" + hex, 512);
    expect(p.text.split("\n")).toEqual([
      Array(16).fill("ab").join(" "),
      Array(4).fill("ab").join(" "),
    ]);
  });

  it("caps at maxBytes and says so with the panel's standing word", () => {
    var hex = Array(600).fill("ff").join("");
    var p = value.dbHexPreview("\\x" + hex, 512);
    expect(p.bytes).toBe(600);
    expect(p.truncated).toBe(true);
    // 512 byte pairs survive the cap — line breaks are whitespace too, so split on any.
    expect(p.text.split(/\s+/).length).toBe(512);
  });

  it("marks nothing truncated at exactly the cap", () => {
    var hex = Array(512).fill("01").join("");
    var p = value.dbHexPreview("\\x" + hex, 512);
    expect(p.truncated).toBe(false);
    expect(p.bytes).toBe(512);
  });
});

/* The owner's report (2026-09-28): the JSON value read as a folding tree - sans-serif keys, a
   click per level - unlike every other JSON on the panel. The sheet now draws the library's code
   block (ui/json-view.ts): standard JSON, two-space indent, coloured by token. */
describe("dbJsonValueNode — the panel's JSON code block", () => {
  it("prints the whole document as one highlighted code block, no folds", () => {
    var out = html({ a: 1, b: { c: "x" } });
    expect(out.startsWith('<div class="db-val-json"><pre class="jv">')).toBe(true);
    expect(out).not.toContain("<details");
    expect(out).toContain('<span class="jv-k">&quot;a&quot;</span>');
    expect(out).toContain('<span class="jv-n">1</span>');
    expect(out).toContain('<span class="jv-k">&quot;c&quot;</span>');
    expect(out).toContain('<span class="jv-s">&quot;x&quot;</span>');
  });

  it("colours null and booleans as literals and keeps an empty container as its brackets", () => {
    var out = html({ a: null, c: true, none: {}, empty: [] });
    expect(out).toContain('<span class="jv-l">null</span>');
    expect(out).toContain('<span class="jv-l">true</span>');
    expect(out).toContain('<span class="jv-p">{}</span>');
    expect(out).toContain('<span class="jv-p">[]</span>');
  });

  it("keeps keys and string values inert — a document full of tags stays text nodes", () => {
    var out = html({ "<k>": "<script>alert(1)</script>" });
    expect(out).not.toContain("<script>");
    expect(out).not.toContain("<k>");
    expect(out).toContain("&lt;k&gt;");
    expect(out).toContain("&lt;script&gt;");
  });

  it("shows a string that holds JSON as the JSON it holds, behind the decoded marker", () => {
    var out = html({ data: '{"px":1.5}' });
    expect(out).toContain('<span class="jv-dec">');
    expect(out).toContain('<span class="jv-k">&quot;px&quot;</span>');
    expect(out).toContain('<span class="jv-n">1.5</span>');
  });

  it("paints the first lines of a long document and swaps in the rest on Show all", () => {
    var big = Array.from({ length: 300 }, (_, i) => { return i; });
    var node = value.dbJsonValueNode(big) as FakeNode;
    var numbers = (): number => { return walk(node).filter((n) => { return n.className === "jv-n"; }).length; };
    var button = walk(node).find((c) => { return c.tag === "button"; });
    expect(numbers(), "the first JV_LINES lines only").toBeLessThan(300);
    expect(button, "a Show all button").toBeTruthy();
    expect(serialize(button!)).toContain("302");
    button!.onclick!();
    expect(numbers(), "every line after Show all").toBe(300);
    expect(walk(node).some((c) => { return c.tag === "button"; }), "the button leaves with its job done").toBe(false);
  });
});
