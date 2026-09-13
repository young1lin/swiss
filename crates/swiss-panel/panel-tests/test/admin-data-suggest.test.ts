import { describe, it, expect } from "vitest";
import { join, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

// Same DOM-stub technique as admin-data-grep.test.ts: the panel ships browser ES modules, so
// the module graph needs the globals stubbed before it will evaluate under Node.
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
  confirm: () => true, alert: () => {}, prompt: () => "",
  Blob: class {}, setInterval: () => 0, clearInterval: () => {},
  addEventListener: () => {}, removeEventListener: () => {},
});

const sug = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "js", "data-suggest.js")).href
) as {
  dbSuggestPrefixAt: (text: string, caret: number) => string;
  dbSuggestByteOffset: (text: string, caret: number) => number;
};

// docs/22 W3.1: the panel's half of the completion contract — the word class matches the
// server's sql_word_ending_at, and the caret crosses the wire as a byte offset.
describe("dbSuggestPrefixAt", () => {
  it("reads the identifier (dots and dollars included) ending at the caret", () => {
    expect(sug.dbSuggestPrefixAt("SELECT * FROM public.us", 23)).toBe("public.us");
    expect(sug.dbSuggestPrefixAt("SELECT na", 9)).toBe("na");
    expect(sug.dbSuggestPrefixAt("price$1", 7)).toBe("price$1");
  });

  it("returns nothing over whitespace, punctuation or an empty prefix", () => {
    expect(sug.dbSuggestPrefixAt("SELECT * FROM users ", 20)).toBe("");
    expect(sug.dbSuggestPrefixAt("SELECT 1)", 9)).toBe("");
    expect(sug.dbSuggestPrefixAt("", 0)).toBe("");
  });
});

describe("dbSuggestByteOffset", () => {
  it("is the selection index while the statement is ASCII", () => {
    expect(sug.dbSuggestByteOffset("SELECT * FROM users WHERE u", 27)).toBe(27);
  });

  it("counts bytes once the statement holds multi-byte characters", () => {
    // 6 UTF-16 units before the caret, 10 UTF-8 bytes: each 注/释 is 3 bytes.
    expect(sug.dbSuggestByteOffset("-- 注释\nSEL", 7)).toBe(11);
  });
});
