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
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-suggest.ts")).href
) as {
  dbSuggestPrefixAt: (text: string, caret: number) => string;
  dbSuggestByteOffset: (text: string, caret: number) => number;
  dbRedisWordAt: (text: string, caret: number) => string;
  dbRedisCandidates: (text: string, caret: number, keys: string[]) => { label: string; kind: string; detail?: string }[];
  REDIS_TEMPLATES: { group: string; line: string }[];
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

/* The owner (2026-09-28): "when I type a Redis command I need completion, and templates -
   otherwise there is no way to know how to set a key, or delete one." The SQL console asks the
   server for its candidates and skips redis entirely; redis needs no round trip, because the
   command set is fixed and the key names are already in the sidebar. */
describe("dbRedisWordAt", () => {
  it("takes the key characters a redis word is made of, not the SQL ones", () => {
    // A key is a byte string: colons group it, dashes and dots are ordinary, * is a pattern.
    expect(sug.dbRedisWordAt("GET user:1", 10)).toBe("user:1");
    expect(sug.dbRedisWordAt("SCAN 0 MATCH sess-", 18)).toBe("sess-");
    expect(sug.dbRedisWordAt("KEYS market:*", 13)).toBe("market:*");
    expect(sug.dbRedisWordAt("HGETALL ", 8)).toBe("");
  });
});

describe("dbRedisCandidates", () => {
  const keys = ["user:1", "user:2", "market:ticks", "htest"];
  const labels = (text: string, caret = text.length): string[] =>
    sug.dbRedisCandidates(text, caret, keys).map((c) => c.label);

  it("completes the command word, and says what arguments it takes", () => {
    expect(labels("SE")).toContain("SET");
    expect(labels("se"), "typed lower case, offered upper").toContain("SET");
    const set = sug.dbRedisCandidates("SE", 2, keys).find((c) => c.label === "SET")!;
    expect(set.detail).toBe("SET key value [EX seconds]");
    expect(set.kind).toBe("string");
    // The two the owner could not find a way to write.
    expect(labels("DE")).toContain("DEL");
    expect(labels("EXP")).toContain("EXPIRE");
  });

  it("completes a key once the command is one that takes a key", () => {
    expect(labels("GET user:")).toEqual(["user:1", "user:2"]);
    expect(labels("DEL market")).toEqual(["market:ticks"]);
    expect(sug.dbRedisCandidates("GET user:", 9, keys)[0].kind).toBe("key");
  });

  it("offers nothing where nothing can be completed", () => {
    expect(labels("GET user:1 ")).toEqual([]);      // past the key, over whitespace
    expect(labels("SET user:1 val")).toEqual([]);   // a VALUE is the operator's, never guessed
    expect(labels("INFO keysp")).toEqual([]);       // INFO takes no key
    expect(labels("")).toEqual([]);
  });

  it("is a closed set: every template's command is one the completion knows", () => {
    const names = sug.dbRedisCandidates("", 0, []).length;
    expect(names, "an empty prefix offers no list").toBe(0);
    const known = new Set(sug.REDIS_TEMPLATES.map((t) => t.line.split(" ")[0]));
    for (const name of known) {
      expect(labels(name.slice(0, 2)), name + " completes").toContain(name);
    }
  });
});

describe("REDIS_TEMPLATES", () => {
  it("covers writing, reading, expiring and deleting a key of every type", () => {
    const lines = sug.REDIS_TEMPLATES.map((t) => t.line);
    expect(lines).toContain("SET key value");
    expect(lines).toContain("DEL key");
    expect(lines).toContain("EXPIRE key 60");
    expect(lines).toContain("HSET key field value");
    expect(lines).toContain("XADD key * field value");
    expect(new Set(sug.REDIS_TEMPLATES.map((t) => t.group)).size, "grouped by what they act on")
      .toBeGreaterThan(3);
  });

  it("every template is a line the console can run as it stands", () => {
    for (const t of sug.REDIS_TEMPLATES) {
      expect(t.line, "no placeholder braces - the words ARE the placeholders").not.toMatch(/[<>{}]/);
      expect(t.line.trim()).toBe(t.line);
    }
  });
});
