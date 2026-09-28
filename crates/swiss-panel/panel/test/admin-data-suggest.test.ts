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

interface RedisCmdFixture {
  name: string; syntax: string; summary: string; since: string; group: string;
  tokens: string[]; container: boolean;
  arity?: number; firstKey?: number; lastKey?: number; step?: number;
}

const sug = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-suggest.ts")).href
) as {
  dbSuggestPrefixAt: (text: string, caret: number) => string;
  dbSuggestByteOffset: (text: string, caret: number) => number;
  dbRedisWordAt: (text: string, caret: number) => string;
  dbRedisCandidates: (text: string, caret: number, keys: string[], cmds: RedisCmdFixture[]) => { label: string; kind: string; detail?: string }[];
  dbRedisLineAt: (text: string, caret: number) => { words: string[]; index: number };
  dbRedisCmdOf: (words: string[], cmds: RedisCmdFixture[]) => RedisCmdFixture | null;
  dbRedisKeyAt: (cmd: RedisCmdFixture, index: number, words: number) => boolean;
  dbRedisSignature: (text: string, caret: number, cmds: RedisCmdFixture[]) => string;
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

/* docs/50: the completion is built from the catalog the SERVER answered with, so these
   fixtures are what COMMAND DOCS + COMMAND INFO produce for a handful of commands - the
   same rows redis_commands returns over the wire. */
const CAT = [
  { name: "SET", syntax: "key value [NX|XX] [EX seconds]", summary: "Set the string value of a key",
    since: "1.0.0", group: "string", tokens: ["NX", "XX", "EX"], container: false,
    arity: -3, firstKey: 1, lastKey: 1, step: 1 },
  { name: "GET", syntax: "key", summary: "Get the value of a key", since: "1.0.0",
    group: "string", tokens: [], container: false, arity: 2, firstKey: 1, lastKey: 1, step: 1 },
  { name: "DEL", syntax: "key [key ...]", summary: "Delete a key", since: "1.0.0",
    group: "generic", tokens: [], container: false, arity: -2, firstKey: 1, lastKey: -1, step: 1 },
  { name: "MSET", syntax: "key value [key value ...]", summary: "Set several keys", since: "1.0.1",
    group: "string", tokens: [], container: false, arity: -3, firstKey: 1, lastKey: -1, step: 2 },
  { name: "SCAN", syntax: "cursor [MATCH pattern] [COUNT count]", summary: "Iterate the keyspace",
    since: "2.8.0", group: "generic", tokens: ["MATCH", "COUNT"], container: false,
    arity: -2, firstKey: 0, lastKey: 0, step: 0 },
  // A container's OWN key positions are zeros - redis 7 puts the real ones on each
  // subcommand's row, which is what the panel reads (docs/50 §2.3).
  { name: "XINFO", syntax: "", summary: "A container for stream introspection", since: "5.0.0",
    group: "stream", tokens: [], container: true, arity: -2, firstKey: 0, lastKey: 0, step: 0 },
  { name: "XINFO GROUPS", syntax: "key", summary: "List the consumer groups", since: "5.0.0",
    group: "stream", tokens: [], container: false, firstKey: 2, lastKey: 2, step: 1 },
  { name: "XINFO STREAM", syntax: "key [FULL]", summary: "Get information about a stream",
    since: "5.0.0", group: "stream", tokens: ["FULL"], container: false, firstKey: 2, lastKey: 2, step: 1 },
  { name: "INFO", syntax: "[section]", summary: "Server information", since: "1.0.0",
    group: "server", tokens: [], container: false, arity: -1, firstKey: 0, lastKey: 0, step: 0 },
];

describe("dbRedisCandidates (docs/50 - from the server's own catalog)", () => {
  const keys = ["user:1", "user:2", "market:ticks", "htest"];
  const labels = (text: string, caret = text.length): string[] =>
    sug.dbRedisCandidates(text, caret, keys, CAT).map((c) => c.label);

  it("completes the command word, and says what it takes AND what it does", () => {
    expect(labels("SE")).toContain("SET");
    expect(labels("se"), "typed lower case, offered upper").toContain("SET");
    const set = sug.dbRedisCandidates("SE", 2, keys, CAT).find((c) => c.label === "SET")!;
    expect(set.detail, "the syntax first - it is what gets typed next - then the summary")
      .toBe("SET key value [NX|XX] [EX seconds] — Set the string value of a key");
    expect(set.kind).toBe("string");
    expect(labels("DE")).toContain("DEL");
  });

  it("completes a key at every position the server says is a key", () => {
    expect(labels("GET user:")).toEqual(["user:1", "user:2"]);
    expect(labels("DEL market")).toEqual(["market:ticks"]);
    // DEL takes keys to the end of the line: the third word is a key too.
    expect(labels("DEL user:1 user:")).toEqual(["user:1", "user:2"]);
    // MSET steps by two - word 1 and word 3 are keys, word 2 is a value nobody guesses.
    expect(labels("MSET user:1 v user:")).toEqual(["user:1", "user:2"]);
    expect(labels("MSET user:1 user:"), "the VALUE slot offers no keys").toEqual([]);
    expect(sug.dbRedisCandidates("GET user:", 9, keys, CAT)[0].kind).toBe("key");
  });

  it("completes a container's subcommands, and then the subcommand's own key", () => {
    expect(labels("XINFO "), "in the catalog's own order, which the server sorted").toEqual(["GROUPS", "STREAM"]);
    expect(labels("XINFO STR")).toEqual(["STREAM"]);
    expect(labels("XINFO STREAM market"), "the key sits one word later here").toEqual(["market:ticks"]);
  });

  it("completes the tokens a command accepts, once, where a key does not belong", () => {
    expect(labels("SET user:1 v ")).toEqual(["NX", "XX", "EX"]);
    expect(labels("SET user:1 v N")).toEqual(["NX"]);
    expect(labels("SET user:1 v NX "), "a token already typed is not offered again")
      .toEqual(["XX", "EX"]);
    expect(labels("SCAN 0 MA")).toEqual(["MATCH"]);
    expect(sug.dbRedisCandidates("SCAN 0 MA", 9, keys, CAT)[0].kind).toBe("token");
  });

  it("offers nothing where nothing can be completed", () => {
    expect(labels("GET user:1 "), "GET takes one key and no tokens").toEqual([]);
    expect(labels("INFO keysp"), "INFO takes no key and no tokens").toEqual([]);
    expect(labels("NOSUCHCOMMAND arg")).toEqual([]);
    expect(labels("", 0)).toEqual([]);
    expect(sug.dbRedisCandidates("GET u", 5, keys, []), "no catalog, no guesses").toEqual([]);
  });

  it("reads the LINE the caret is on, not the whole box", () => {
    const text = "GET user:1\nDEL mark";
    expect(labels(text)).toEqual(["market:ticks"]);
    expect(sug.dbRedisLineAt(text, text.length).words).toEqual(["DEL", "mark"]);
    expect(sug.dbRedisLineAt(text, text.length).index).toBe(1);
  });
});

describe("dbRedisKeyAt (docs/50 - the three numbers redis-cli reads)", () => {
  const of = (name: string) => CAT.find((c) => c.name === name)!;
  it("says which words of a line are key names", () => {
    expect(sug.dbRedisKeyAt(of("GET"), 1, 2)).toBe(true);
    expect(sug.dbRedisKeyAt(of("GET"), 2, 3), "one key, and it is word 1").toBe(false);
    expect(sug.dbRedisKeyAt(of("DEL"), 3, 4), "lastKey -1: to the end of the line").toBe(true);
    expect(sug.dbRedisKeyAt(of("MSET"), 1, 5)).toBe(true);
    expect(sug.dbRedisKeyAt(of("MSET"), 2, 5), "step 2: the values are not keys").toBe(false);
    expect(sug.dbRedisKeyAt(of("MSET"), 3, 5)).toBe(true);
    expect(sug.dbRedisKeyAt(of("SCAN"), 1, 2), "firstKey 0: no key anywhere").toBe(false);
  });
});

describe("dbRedisSignature (docs/50 - the line under the box)", () => {
  it("shows the command being typed, its arguments and what it does", () => {
    expect(sug.dbRedisSignature("SET user:1 ", 11, CAT))
      .toBe("SET key value [NX|XX] [EX seconds] — Set the string value of a key");
    expect(sug.dbRedisSignature("XINFO STREAM market:ticks", 25, CAT))
      .toBe("XINFO STREAM key [FULL] — Get information about a stream");
  });

  it("says nothing about a line it does not recognise, so the standing hint stays", () => {
    expect(sug.dbRedisSignature("", 0, CAT)).toBe("");
    expect(sug.dbRedisSignature("NOSUCH ", 7, CAT)).toBe("");
    expect(sug.dbRedisSignature("SET k v", 7, [])).toBe("");
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
