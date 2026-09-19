/*
 * Copyright 2026 The swiss authors
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
import { dbView } from "../src/db-state.js";

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

const edits = await import(
  pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "data-browsers.ts")).href
) as {
  dbRedisValidScore: (s: string) => boolean;
  dbRedisCommands: (key: string, type: string, buf: unknown) => { verb: string; args: unknown[] }[];
  dbRedisCommandText: (cmd: { verb: string; args: unknown[] }) => string;
  dbRedisEntries: (v: unknown, buf: unknown) => { addr: string; deleted: boolean; updated: boolean; cells: unknown }[];
  dbRedisPendingCount: () => number;
};

// docs/22 W3.3: the buffered typed-value edits fold into the exact command list Commit posts
// (dbgate's ChangeSetRedis order — updates, inserts, deletes), scores and list indexes as
// numbers, and the preview renders the very same list, so what the operator reads is what runs.
describe("dbRedisCommands", () => {
  it("a hash folds to HSET / HDEL in the dbgate order", () => {
    const buf = {
      updates: { keep: "changed" },
      deletes: { gone: 1 },
      inserts: [{ field: "fresh", value: "a value with spaces" }],
    };
    expect(edits.dbRedisCommands("h:1", "hash", buf)).toEqual([
      { verb: "HSET", args: ["h:1", "keep", "changed"] },
      { verb: "HSET", args: ["h:1", "fresh", "a value with spaces"] },
      { verb: "HDEL", args: ["h:1", "gone"] },
    ]);
  });

  it("a zset carries scores as numbers, a list addresses indexes as numbers", () => {
    const z = edits.dbRedisCommands("z:1", "zset", {
      updates: { alice: "1.5" },
      deletes: {},
      inserts: [{ member: "bob", score: "2" }],
    });
    expect(z).toEqual([
      { verb: "ZADD", args: ["z:1", 1.5, "alice"] },
      { verb: "ZADD", args: ["z:1", 2, "bob"] },
    ]);
    const l = edits.dbRedisCommands("l:1", "list", {
      updates: { "1": "rewritten" },
      deletes: {},
      inserts: [{ value: "tail" }],
    });
    expect(l).toEqual([
      { verb: "LSET", args: ["l:1", 1, "rewritten"] },
      { verb: "RPUSH", args: ["l:1", "tail"] },
    ]);
  });

  it("a set only inserts and deletes — there is nothing to update", () => {
    expect(edits.dbRedisCommands("s:1", "set", {
      updates: {},
      deletes: { stale: 1 },
      inserts: [{ member: "new" }],
    })).toEqual([
      { verb: "SADD", args: ["s:1", "new"] },
      { verb: "SREM", args: ["s:1", "stale"] },
    ]);
  });

  it("a blank insert address or a bad score refuses instead of dropping silently", () => {
    expect(() => edits.dbRedisCommands("h:1", "hash", {
      updates: {}, deletes: {}, inserts: [{ field: "  ", value: "x" }],
    })).toThrow(/field needs a name/);
    expect(() => edits.dbRedisCommands("z:1", "zset", {
      updates: { a: "soon" }, deletes: {}, inserts: [],
    })).toThrow(/not a valid score/);
    expect(() => edits.dbRedisCommands("z:1", "zset", {
      updates: {}, deletes: {}, inserts: [{ member: "m", score: "nan" }],
    })).toThrow(/not a valid score/);
  });

  it("inf spellings pass the score gate and travel as strings", () => {
    expect(edits.dbRedisValidScore("inf")).toBe(true);
    expect(edits.dbRedisValidScore("-inf")).toBe(true);
    expect(edits.dbRedisValidScore("-0.5")).toBe(true);
    expect(edits.dbRedisValidScore("soon")).toBe(false);
    const z = edits.dbRedisCommands("z:1", "zset", {
      updates: { top: "inf" }, deletes: {}, inserts: [],
    });
    expect(z[0].args[1]).toBe("inf");
  });
});

describe("dbRedisCommandText", () => {
  it("quotes strings and escapes their inner quotes, numbers stay bare", () => {
    expect(edits.dbRedisCommandText({ verb: "HSET", args: ["h:1", "note", 'say "hi"'] }))
      .toBe('HSET "h:1" "note" "say \\"hi\\""');
    expect(edits.dbRedisCommandText({ verb: "ZADD", args: ["z:1", 1.5, "alice"] }))
      .toBe('ZADD "z:1" 1.5 "alice"');
  });
});

describe("dbRedisEntries", () => {
  it("applies buffered updates and marks deletes on the row shape the table paints", () => {
    const v = { type: "hash", value: { a: "1", b: "2", c: "3" } };
    const rows = edits.dbRedisEntries(v, { updates: { b: "two" }, deletes: { c: 1 }, inserts: [] });
    expect(rows).toEqual([
      { addr: "a", deleted: false, updated: false, cells: { field: "a", value: "1" } },
      { addr: "b", deleted: false, updated: true, cells: { field: "b", value: "two" } },
      { addr: "c", deleted: true, updated: false, cells: { field: "c", value: "3" } },
    ]);
  });

  it("a zset shows the pending score and a list keeps its index addresses", () => {
    const z = edits.dbRedisEntries({ type: "zset", value: { alice: 1.5 } }, { updates: { alice: "2" }, deletes: {}, inserts: [] });
    expect(z[0].cells).toEqual({ member: "alice", score: "2" });
    const l = edits.dbRedisEntries({ type: "list", value: ["a", "b"] }, { updates: { "1": "B" }, deletes: {}, inserts: [] });
    expect(l[1]).toEqual({ addr: "1", deleted: false, updated: true, cells: { index: "1", value: "B" } });
  });
});

describe("dbRedisPendingCount", () => {
  it("counts updates, deletes and inserts over the state's buffer", async () => {
    const { state } = await import(
      pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "..", "src", "util.ts")).href
    ) as { state: { db: Record<string, unknown> } };
    expect(edits.dbRedisPendingCount()).toBe(0);
    dbView().redisEdits = { key: "k", type: "set", updates: {}, deletes: { a: 1 }, inserts: [{ member: "x" }, { member: "y" }] };
    expect(edits.dbRedisPendingCount()).toBe(3);
    dbView().redisEdits = null;
  });
});
