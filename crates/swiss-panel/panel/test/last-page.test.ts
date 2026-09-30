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

import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { LAST_PAGE_KEY, loadLastPages, rememberLastPage, targetPageFor } from "../src/last-page.js";

/* The seat's target (SPEC §panel.nav): a pure function over (group, memory, usability) plus a
 * localStorage-backed pair. Node has no storage global, so this suite installs one per test -
 * including the hostile variants (garbage JSON, a throwing getItem/setItem) the browser can
 * produce via private mode or a full quota. What must hold everywhere: the memory never
 * raises, and a broken store degrades to the empty map, never to a lost click. */

const mcp = { id: "mcp", pages: [{ id: "mcps" }, { id: "traffic" }, { id: "tokens" }] };

const memory = () => {
  const map = new Map<string, string>();
  return {
    getItem: (k: string) => (map.has(k) ? map.get(k) as string : null),
    setItem: (k: string, v: string) => { map.set(k, v); },
    removeItem: (k: string) => { map.delete(k); },
    dump: () => map,
  };
};
const install = (storage: unknown): void => {
  Object.defineProperty(globalThis, "localStorage", { value: storage, configurable: true });
};
let previous: unknown;

beforeEach(() => { previous = (globalThis as Record<string, unknown>).localStorage; });
afterEach(() => { install(previous); });

describe("targetPageFor", () => {
  it("reopens the remembered page when it is still a member and usable", () => {
    expect(targetPageFor(mcp, { mcp: "traffic" }, () => true)).toBe("traffic");
  });
  it("an empty memory falls back to the group's first page", () => {
    expect(targetPageFor(mcp, {}, () => true)).toBe("mcps");
  });
  it("a remembered page that left the group is ignored (a plugin dropped a page)", () => {
    expect(targetPageFor(mcp, { mcp: "jobs" }, () => true)).toBe("mcps");
  });
  it("a remembered page that is currently unavailable is ignored", () => {
    expect(targetPageFor(mcp, { mcp: "traffic" }, (id) => id !== "traffic")).toBe("mcps");
  });
  it("a group with no pages targets nothing rather than raising", () => {
    expect(targetPageFor({ id: "x", pages: [] }, { x: "y" }, () => true)).toBe("");
  });
});

describe("the store", () => {
  it("loads {} when nothing was ever stored, and when there is no storage at all", () => {
    install(memory());
    expect(loadLastPages()).toEqual({});
    install(undefined);
    expect(loadLastPages()).toEqual({});
  });
  it("garbage JSON degrades to the empty map", () => {
    install({ getItem: () => "{nope", setItem: () => {} });
    expect(loadLastPages()).toEqual({});
  });
  it("non-object JSON (an array, a string) degrades to the empty map", () => {
    install({ getItem: () => "[\"mcp\"]", setItem: () => {} });
    expect(loadLastPages()).toEqual({});
    install({ getItem: () => "\"mcp\"", setItem: () => {} });
    expect(loadLastPages()).toEqual({});
  });
  it("a throwing getItem degrades to the empty map", () => {
    install({ getItem: () => { throw new Error("blocked"); }, setItem: () => {} });
    expect(loadLastPages()).toEqual({});
  });
  it("rememberLastPage merges into what is there and round-trips", () => {
    const store = memory();
    install(store);
    store.setItem(LAST_PAGE_KEY, JSON.stringify({ tunnels: "tunnel-forwards" }));
    rememberLastPage("mcp", "traffic");
    expect(loadLastPages()).toEqual({ tunnels: "tunnel-forwards", mcp: "traffic" });
    // A later visit replaces the same plugin's entry, not the whole map.
    rememberLastPage("mcp", "tokens");
    expect(loadLastPages()).toEqual({ tunnels: "tunnel-forwards", mcp: "tokens" });
  });
  it("a throwing setItem never bubbles - the click survives without its memory", () => {
    install({ getItem: () => null, setItem: () => { throw new Error("quota"); } });
    expect(() => rememberLastPage("mcp", "traffic")).not.toThrow();
  });
  it("non-string values in the stored map are dropped on load", () => {
    install({ getItem: () => JSON.stringify({ mcp: "traffic", bad: 3 }), setItem: () => {} });
    expect(loadLastPages()).toEqual({ mcp: "traffic" });
  });
});
