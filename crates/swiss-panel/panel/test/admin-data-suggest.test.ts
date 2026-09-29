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
import { readFileSync } from "node:fs";
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
  keySpecs?: unknown[];
}

const sug = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-suggest.ts")).href
) as {
  dbSuggestPrefixAt: (text: string, caret: number) => string;
  dbSuggestByteOffset: (text: string, caret: number) => number;
  dbRedisWordSpan: (text: string, caret: number) => { start: number; word: string };
  dbRedisQuoteArg: (s: string) => string;
  dbRedisCandidates: (text: string, caret: number, keys: string[], cmds: RedisCmdFixture[]) => { label: string; kind: string; detail?: string; insert?: string }[];
  dbRedisLineAt: (text: string, caret: number) => { words: string[]; index: number };
  dbRedisCmdOf: (words: string[], cmds: RedisCmdFixture[]) => RedisCmdFixture | null;
  dbRedisKeyAt: (cmd: RedisCmdFixture, index: number, words: string[]) => boolean;
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
describe("dbRedisWordSpan", () => {
  const at = (text: string): { start: number; word: string } => sug.dbRedisWordSpan(text, text.length);
  it("takes the word the server will read, not the SQL identifier class", () => {
    // A key is a byte string: colons group it, dashes and dots are ordinary, * is a pattern.
    expect(at("GET user:1")).toEqual({ start: 4, word: "user:1" });
    expect(at("SCAN 0 MATCH sess-")).toEqual({ start: 13, word: "sess-" });
    expect(at("KEYS market:*")).toEqual({ start: 5, word: "market:*" });
    expect(at("HGETALL ")).toEqual({ start: 8, word: "" });
  });
  it("a name in any script is one word - the old character class stopped at 用 (2026-09-29)", () => {
    expect(at("GET 用户")).toEqual({ start: 4, word: "用户" });
    expect(at("GET 🔑:")).toEqual({ start: 4, word: "🔑:" });
    expect(at("GET #tag")).toEqual({ start: 4, word: "#tag" });
  });
  it("an open quote or an escaped blank is inside the word, and the span starts at its raw start", () => {
    expect(at("GET \"my k")).toEqual({ start: 4, word: "my k" });
    expect(at("GET my\\ k")).toEqual({ start: 4, word: "my k" });
    expect(at("GET 'a b")).toEqual({ start: 4, word: "a b" });
    expect(at("SET k \"a b\" c")).toEqual({ start: 12, word: "c" });
    expect(at("  GE")).toEqual({ start: 2, word: "GE" });
  });
  it("counts from the caret's own line", () => {
    expect(at("GET a\nGET 用")).toEqual({ start: 10, word: "用" });
    expect(at("GET a\n")).toEqual({ start: 6, word: "" });
  });
  it("a line shlex cannot read falls back to the blank-bounded run", () => {
    // A trailing backslash: no split reads it, so the span is the blank-bounded tail.
    expect(at("SET \"a b\\").start).toBe(7);
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
  const w = (n: number): string[] => Array.from({ length: n }, (_, i) => "w" + i);
  it("says which words of a line are key names", () => {
    expect(sug.dbRedisKeyAt(of("GET"), 1, w(2))).toBe(true);
    expect(sug.dbRedisKeyAt(of("GET"), 2, w(3)), "one key, and it is word 1").toBe(false);
    expect(sug.dbRedisKeyAt(of("DEL"), 3, w(4)), "lastKey -1: to the end of the line").toBe(true);
    expect(sug.dbRedisKeyAt(of("MSET"), 1, w(5))).toBe(true);
    expect(sug.dbRedisKeyAt(of("MSET"), 2, w(5)), "step 2: the values are not keys").toBe(false);
    expect(sug.dbRedisKeyAt(of("MSET"), 3, w(5))).toBe(true);
    expect(sug.dbRedisKeyAt(of("SCAN"), 1, w(2)), "firstKey 0: no key anywhere").toBe(false);
    expect(sug.dbRedisKeyAt(of("GET"), 0, w(1)), "word 0 is the command, never a key").toBe(false);
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

/* The commands whose keys MOVE, as redis 7 describes them in COMMAND INFO's key specs: XREAD's
   keys follow STREAMS and stop where its ids start, ZUNION's follow a count, EVAL's follow the
   script and a count, ZUNIONSTORE has a destination AND a counted list. first/last/step say
   "no key" (or only the destination) for all of them, so before the specs these never
   completed a key at all. */
const MOVING = [
  { name: "XREAD", syntax: "[COUNT count] [BLOCK milliseconds] STREAMS key [key ...] id [id ...]",
    summary: "Read streams", since: "5.0.0", group: "stream", tokens: ["COUNT", "BLOCK", "STREAMS"],
    container: false, arity: -4, firstKey: 0, lastKey: 0, step: 0,
    keySpecs: [{ begin: { type: "keyword", keyword: "STREAMS", startfrom: 1 }, find: { type: "range", lastkey: -1, keystep: 1, limit: 2 } }] },
  { name: "ZUNION", syntax: "numkeys key [key ...]", summary: "Union", since: "6.2.0", group: "sorted-set",
    tokens: ["WITHSCORES"], container: false, arity: -3, firstKey: 0, lastKey: 0, step: 0,
    keySpecs: [{ begin: { type: "index", index: 1 }, find: { type: "keynum", keynumidx: 0, firstkey: 1, keystep: 1 } }] },
  { name: "ZUNIONSTORE", syntax: "destination numkeys key [key ...]", summary: "Union into", since: "2.0.0",
    group: "sorted-set", tokens: [], container: false, arity: -4, firstKey: 1, lastKey: 1, step: 1,
    keySpecs: [
      { begin: { type: "index", index: 1 }, find: { type: "range", lastkey: 0, keystep: 1, limit: 0 } },
      { begin: { type: "index", index: 2 }, find: { type: "keynum", keynumidx: 0, firstkey: 1, keystep: 1 } },
    ] },
  { name: "EVAL", syntax: "script numkeys [key [key ...]] [arg [arg ...]]", summary: "Run a script",
    since: "2.6.0", group: "scripting", tokens: [], container: false, arity: -3, firstKey: 0, lastKey: 0, step: 0,
    keySpecs: [{ begin: { type: "index", index: 2 }, find: { type: "keynum", keynumidx: 0, firstkey: 1, keystep: 1 } }] },
  // A module command: the dot is part of the name, and the catalog says where its key is.
  { name: "JSON.SET", syntax: "key path value [NX|XX]", summary: "Set a JSON value", since: "1.0.0",
    group: "module", tokens: ["NX", "XX"], container: false, arity: -4, firstKey: 1, lastKey: 1, step: 1 },
];

describe("keys that move: redis 7's key specs (2026-09-29)", () => {
  const keys = ["market:ticks", "market:fast", "user:1"];
  const labels = (text: string, caret = text.length): string[] =>
    sug.dbRedisCandidates(text, caret, keys, [...CAT, ...MOVING]).map((c) => c.label);

  it("XREAD: the words after STREAMS are keys until the ids start", () => {
    expect(labels("XREAD STREAMS market:t")).toEqual(["market:ticks"]);
    expect(labels("XREAD COUNT 10 BLOCK 0 STREAMS mar")).toEqual(["market:ticks", "market:fast"]);
    // After one key the next word is either a second key or the first id - nobody can know
    // while the line is unfinished. A prefix that starts like a key name is offered keys;
    // a bare blank, or an id (`0`, `$`), is offered none.
    expect(labels("XREAD STREAMS market:ticks market:f"), "typing a second key").toEqual(["market:fast"]);
    expect(labels("XREAD STREAMS market:ticks "), "a blank there is not flooded with keys").toEqual([]);
    expect(labels("XREAD STREAMS market:ticks 0"), "an id").toEqual([]);
    expect(labels("XREAD STREAMS market:ticks $"), "the other id").toEqual([]);
    // With the ids typed, the key half is settled: word 2 is a key, words 4 and 5 are not.
    expect(sug.dbRedisKeyAt(MOVING[0], 2, ["XREAD", "STREAMS", "a", "b", "0", "0"])).toBe(true);
    expect(sug.dbRedisKeyAt(MOVING[0], 3, ["XREAD", "STREAMS", "a", "b", "0", "0"])).toBe(true);
    expect(sug.dbRedisKeyAt(MOVING[0], 4, ["XREAD", "STREAMS", "a", "b", "0", "0"])).toBe(false);
    expect(labels("XREAD COUNT mar"), "before STREAMS nothing is a key").toEqual([]);
  });

  it("ZUNION / EVAL: a count says how many keys follow, and nothing past it is one", () => {
    expect(labels("ZUNION 2 mar")).toEqual(["market:ticks", "market:fast"]);
    expect(labels("ZUNION 2 market:ticks mar")).toEqual(["market:ticks", "market:fast"]);
    expect(labels("ZUNION 2 market:ticks market:fast mar"), "a third key would be past the count").toEqual([]);
    expect(labels("ZUNION mar"), "word 1 is the count itself").toEqual([]);
    expect(labels("ZUNION x mar"), "a count that is not a number counts nothing").toEqual([]);
    expect(labels("EVAL \"return 1\" 1 mar"), "the quoted script is ONE word").toEqual(["market:ticks", "market:fast"]);
    expect(labels("EVAL \"return 1\" 0 mar"), "numkeys 0: every word after is an argument").toEqual([]);
  });

  it("ZUNIONSTORE: the destination AND the counted keys", () => {
    expect(labels("ZUNIONSTORE mar")).toEqual(["market:ticks", "market:fast"]);
    expect(labels("ZUNIONSTORE dst 2 us")).toEqual(["user:1"]);
  });

  it("a module command completes by its dotted name and its key", () => {
    expect(labels("json.s")).toEqual(["JSON.SET"]);
    expect(labels("JSON.SET us")).toEqual(["user:1"]);
  });
});

describe("a console line at its edges (2026-09-29)", () => {
  const keys = ["user:1", "user:2", "market:ticks"];
  const labels = (text: string, caret = text.length): string[] =>
    sug.dbRedisCandidates(text, caret, keys, CAT).map((c) => c.label);

  it("leading blanks and tabs are not a word - the key is still word 1", () => {
    // split(/\s+/) made "   GET user:" three words, with the key at index 2: no completion.
    expect(labels("   GET user:")).toEqual(["user:1", "user:2"]);
    expect(labels("\tGET\t\tuser:")).toEqual(["user:1", "user:2"]);
    expect(sug.dbRedisLineAt("   GET user:", 12)).toEqual({ words: ["GET", "user:"], index: 1 });
  });

  it("a quoted argument is one word, the way the server will split it", () => {
    expect(labels('MSET "a b" 1 user:'), "word 3 is a key").toEqual(["user:1", "user:2"]);
    expect(labels('MSET "a b" user:'), "word 2 is a value").toEqual([]);
    expect(sug.dbRedisLineAt("SET 'x y' ", 10)).toEqual({ words: ["SET", "x y", ""], index: 2 });
  });

  it("an open quote is closed for the count: the caret is inside that quoted word", () => {
    expect(sug.dbRedisLineAt('SET "my k', 9)).toEqual({ words: ["SET", "my k"], index: 1 });
    expect(sug.dbRedisLineAt("GET 'a b ", 9), "a blank inside the quote is not a new word").toEqual({ words: ["GET", "a b "], index: 1 });
    expect(labels('GET "user:'), "a key position still completes").toEqual(["user:1", "user:2"]);
  });

  it("an escaped trailing blank belongs to the word, and a trailing backslash does not break the count", () => {
    expect(sug.dbRedisLineAt("GET my\\ ", 8)).toEqual({ words: ["GET", "my "], index: 1 });
    expect(sug.dbRedisLineAt("GET my\\", 7).index, "shlex refuses it; whitespace still counts").toBe(1);
    expect(sug.dbRedisLineAt("GET my\\\\ ", 9), "an escaped backslash, then a real blank").toEqual({ words: ["GET", "my\\", ""], index: 2 });
  });

  it("an empty or blank line is one empty word 0 - and offers nothing", () => {
    expect(sug.dbRedisLineAt("", 0)).toEqual({ words: [""], index: 0 });
    expect(sug.dbRedisLineAt("   ", 3)).toEqual({ words: [""], index: 0 });
    expect(labels("   ")).toEqual([]);
  });

  it("reads up to the caret: a caret mid-line or on an earlier line completes THAT word", () => {
    expect(labels("GET user:1 extra", 9), "the caret after `user:`").toEqual(["user:1", "user:2"]);
    expect(labels("GET us\nDEL market", 6), "the first line, caret at its end").toEqual(["user:1", "user:2"]);
  });

  it("lower case everywhere: command, subcommand and an already-typed token", () => {
    expect(sug.dbRedisCandidates("xinfo stream mar", 16, keys, CAT).map((c) => c.label)).toEqual(["market:ticks"]);
    expect(labels("set k v nx "), "nx typed in lower case is not offered again").toEqual(["XX", "EX"]);
  });

  it("5,000 keys under one prefix: eight offers, at once", () => {
    const many = Array.from({ length: 5000 }, (_, i) => "session:" + i);
    const t0 = Date.now();
    const got = sug.dbRedisCandidates("GET session:", 12, many, CAT);
    expect(got).toHaveLength(8);
    expect(Date.now() - t0, "a keystroke, not a wait").toBeLessThan(100);
  });

  it("a catalog row with nonsense numbers offers nothing rather than guessing", () => {
    const odd = [{ name: "ODD", syntax: "", summary: "", since: "", group: "", tokens: [], container: false,
      firstKey: -3, lastKey: 99, step: 0 }];
    expect(sug.dbRedisCandidates("ODD us", 6, keys, odd)).toEqual([]);
  });
});

describe("a long pasted value costs a keystroke nothing (2026-09-29)", () => {
  // A JSON value pasted inside quotes: the span search used to split the whole line once per
  // blank inside the value - minutes a keystroke at 100 KB. Now only a bounded tail is read.
  const json = (kb: number): string => "{" + Array.from({ length: Math.floor(kb * 1024 / 12) }, (_, i) => '\\"k' + i + '\\": 1').join(", ") + "}";
  for (const kb of [10, 100, 1000]) {
    it(kb + " KB inside an open quote, a closed one, and past it", () => {
      const t0 = Date.now();
      for (const tail of ["", "\"", "\" N"]) {
        const line = "SET k \"" + json(kb) + tail;
        sug.dbRedisWordSpan(line, line.length);
        sug.dbRedisCandidates(line, line.length, ["k"], CAT);
      }
      expect(Date.now() - t0, "three lines, span and candidates each").toBeLessThan(3000);
    }, 30000);
  }
  it("past the value, the next word's span is still exact", () => {
    const line = "SET k \"" + json(100) + "\" N";
    expect(sug.dbRedisWordSpan(line, line.length)).toEqual({ start: line.length - 1, word: "N" });
    // Under the line cap, NX is still offered after the value.
    const short = "SET k \"" + json(10) + "\" N";
    expect(sug.dbRedisCandidates(short, short.length, [], CAT).map((i) => i.label)).toEqual(["NX"]);
  });
  it("a line past 64 KiB offers nothing - it is data being pasted", () => {
    const line = "SET k \"" + json(100) + "\" N";
    expect(sug.dbRedisCandidates(line, line.length, [], CAT)).toEqual([]);
  });
});

describe("a key completion types the name the server will read (2026-09-29)", () => {
  const KEYS = ["用户:1", "user:1", "my key", "my", "say \"hi\"", "a\\b", "", "line\nbreak", "#tag:1"];
  const keysFor = (line: string): { label: string; insert?: string }[] =>
    sug.dbRedisCandidates(line, line.length, KEYS, CAT).filter((i) => i.kind === "key");
  it("GET 用 offers the 用户 names only - it used to offer every key and paste after 用", () => {
    expect(keysFor("GET 用").map((i) => i.label)).toEqual(["用户:1"]);
  });
  it("a name with a blank goes in quoted, or redis would receive two words", () => {
    const my = keysFor("GET my");
    expect(my.map((i) => i.label)).toEqual(["my key", "my"]);
    expect(my[0].insert).toBe("\"my key\"");
    expect(my[1].insert).toBe("my");
    // Typing inside the quote narrows the same way: the word is `my k`.
    expect(keysFor("GET \"my k").map((i) => i.insert)).toEqual(["\"my key\""]);
  });
  it("quotes and backslashes are escaped, and the empty name is shown and typed as \"\"", () => {
    expect(keysFor("GET say").map((i) => i.insert)).toEqual(["\"say \\\"hi\\\"\""]);
    expect(keysFor("GET a").map((i) => i.insert)).toContain("\"a\\\\b\"");
    const blank = keysFor("GET ").find((i) => i.insert === "\"\"");
    expect(blank && blank.label, "the empty name has a visible row").toBe("\"\"");
  });
  it("a name with edge blanks is LISTED quoted too, so ' x' and 'x' are two rows", () => {
    const rows = sug.dbRedisCandidates("GET ", 4, [" x", "x", "x "], CAT);
    expect(rows.map((i) => i.label)).toEqual(['" x"', "x", '"x "']);
    expect(rows.map((i) => i.insert)).toEqual(['" x"', "x", '"x "']);
  });

  it("a name holding a line break is not offered - the console line would run as two commands", () => {
    expect(keysFor("GET line")).toEqual([]);
  });
  it("a name starting with # goes in bare - it is a character, not a comment, on both sides", () => {
    expect(keysFor("GET #").map((i) => i.insert)).toEqual(["#tag:1"]);
  });
});

/* tests/fixtures/console-split.json is read by swiss-host's suite too: the words the server
   hands redis for each line, and the names the completion types. The panel's splitter (the
   vendored JS port) must agree with it word for word, except where an entry says it differs
   and why that moves no word boundary. */
describe("the console splits as the server does (tests/fixtures/console-split.json)", async () => {
  const here = dirname(fileURLToPath(import.meta.url));
  const shlex = await import(
    pathToFileURL(join(here, "..", "..", "src", "admin_assets", "js", "vendor", "shlex", "3.0.0", "index.js")).href
  ) as { split: (s: string) => string[] };
  const corpus = JSON.parse(readFileSync(join(here, "..", "..", "..", "..", "tests", "fixtures", "console-split.json"), "utf8")) as {
    lines: { line: string; words: string[] | null; panel?: string[]; why?: string }[];
    keys: { key: string; typed: string }[];
  };
  it("every line splits into the server's words, or is refused where the server refuses it", () => {
    expect(corpus.lines.length).toBeGreaterThanOrEqual(30);
    for (const c of corpus.lines) {
      const want = c.panel || c.words;
      if (want === null) expect(() => shlex.split(c.line), JSON.stringify(c.line)).toThrow();
      else expect(shlex.split(c.line), JSON.stringify(c.line)).toEqual(want);
    }
  });
  it("where the JS port differs, it differs only in a value's text - never in how many words", () => {
    for (const c of corpus.lines.filter((x) => x.panel)) {
      expect(c.why, JSON.stringify(c.line)).toBeTruthy();
      if (c.line.indexOf("\r") >= 0) continue; // the one a textarea cannot hold
      expect(c.panel!.length, JSON.stringify(c.line)).toBe(c.words!.length);
    }
  });
  it("the completion types each name the way the corpus says, and it splits back to the name", () => {
    for (const k of corpus.keys) {
      expect(sug.dbRedisQuoteArg(k.key), JSON.stringify(k.key)).toBe(k.typed);
      expect(shlex.split("GET " + k.typed), k.typed).toEqual(["GET", k.key]);
    }
  });
});
