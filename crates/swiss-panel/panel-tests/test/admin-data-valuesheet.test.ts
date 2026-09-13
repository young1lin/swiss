import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Same DOM-stub technique as admin-data-cellview.test.ts: the panel ships browser ES modules,
// so the module graph needs the globals stubbed before it will evaluate under Node.
const el = (): Record<string, unknown> => {
  const node: Record<string, unknown> = {
    style: {}, dataset: {}, hidden: false, disabled: false, checked: false, value: "",
    textContent: "", innerHTML: "",
    classList: { add: () => {}, remove: () => {}, toggle: () => {}, contains: () => false },
    setAttribute: () => {}, getAttribute: () => "", removeAttribute: () => {},
    addEventListener: () => {}, removeEventListener: () => {},
    appendChild: (c: unknown) => c, removeChild: (c: unknown) => c, remove: () => {},
    querySelector: () => null, querySelectorAll: () => [],
    focus: () => {}, blur: () => {}, click: () => {},
    getBoundingClientRect: () => ({ left: 0, top: 0, right: 0, bottom: 0, width: 0, height: 0 }),
    contains: () => false, closest: () => null, insertAdjacentHTML: () => {},
  };
  node.cloneNode = () => el();
  return node;
};
const doc = {
  documentElement: el(), body: el(), head: el(),
  hidden: false, visibilityState: "visible", activeElement: null,
  getElementById: () => el(), createElement: () => el(), createTextNode: () => el(),
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
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "data-value.js")).href
) as {
  dbValueKind: (v: unknown, colType: string | null | undefined) => string;
  dbJsonTreeHtml: (v: unknown) => string;
  dbHexPreview: (v: string, maxBytes: number) => { text: string; bytes: number; truncated: boolean };
};

/* docs/22 W5.3 — the value sheet. The presenter choice, the hex cap and the JSON tree are
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

describe("dbJsonTreeHtml — the details-folded tree", () => {
  it("renders the root open and nested containers closed", () => {
    var html = value.dbJsonTreeHtml({ a: 1, b: { c: "x" } });
    expect(html).toContain('<details class="db-val-node" open>');
    expect(html.match(/<details/g)!.length).toBe(2);
    expect(html.match(/<details class="db-val-node" open>/g)!.length).toBe(1); // the root only
    expect(html).toContain('<summary>{ 2 }</summary>');
    expect(html).toContain('<summary>{ 1 }</summary>');
  });

  it("words arrays as [ n ] and empty containers without a count", () => {
    var html = value.dbJsonTreeHtml({ list: [1, 2, 3], none: {}, empty: [] });
    expect(html).toContain("<summary>[ 3 ]</summary>");
    expect(html).toContain("<summary>{ }</summary>");
    expect(html).toContain("<summary>[ ]</summary>");
  });

  it("escapes keys and string values — a document full of tags stays inert text", () => {
    var html = value.dbJsonTreeHtml({ "<k>": "<script>alert(1)</script>" });
    expect(html).not.toContain("<script>");
    expect(html).not.toContain("<k>");
    expect(html).toContain("&lt;k&gt;");
    expect(html).toContain("&lt;script&gt;");
  });

  it("renders null with the panel's NULL styling, numbers and booleans plain, strings quoted", () => {
    var html = value.dbJsonTreeHtml({ a: null, b: 1.5, c: true, d: "hi" });
    expect(html).toContain('<span class="db-val-v db-null">null</span>');
    expect(html).toContain('<span class="db-val-v">1.5</span>');
    expect(html).toContain('<span class="db-val-v">true</span>');
    expect(html).toContain('<span class="db-val-v">&quot;hi&quot;</span>');
  });

  it("array children carry no key row — the indent is the position", () => {
    var html = value.dbJsonTreeHtml([1, "two"]);
    expect(html).not.toContain("db-val-k");
    expect(html).toContain('<span class="db-val-v">1</span>');
    expect(html).toContain("&quot;two&quot;");
  });
});
