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

import { describe, it, expect, beforeAll } from "vitest";

/* docs/24 P3: every client-facing URL the panel can produce goes through connect.js's
   endpointUrl — the single exit point for MCP endpoint addresses. The /mcp/ prefix lives
   there and in the two places that DISPLAY an endpoint path (the sidebar tooltip, the
   traffic clients' path column): if any of these regress to a root-level path, a copied
   client config or a read chip silently points at the retired route. */

// eslint-disable-next-line @typescript-eslint/no-explicit-any
let mods: any;

/** An anything-you-ask-for node: the import graph wires a few top-level DOM handles
 *  (add-sheet.js grabs #addBtn at import time). This suite asserts pure functions, so the
 *  stub only has to let those imports survive; it claims nothing about DOM behavior. */
function permissive(): unknown {
  return new Proxy(function () {} as unknown as Record<string, unknown>, {
    get: () => permissive(),
    set: () => true,
    apply: () => permissive(),
  });
}

beforeAll(async () => {
  const g = globalThis as unknown as Record<string, unknown>;
  g.document = {
    addEventListener() {},
    getElementById: () => permissive(),
    createElement: () => permissive(),
    querySelector: () => null,
    querySelectorAll: () => [],
    body: permissive(),
    documentElement: permissive(),
    hidden: false,
  };
  g.window = { addEventListener() {}, innerWidth: 1440, innerHeight: 900 };
  g.location = { origin: "http://127.0.0.1:19999" };
  g.localStorage = { getItem: () => null, setItem() {} };
  g.fetch = async () => ({ ok: true, status: 200, json: async () => ({}) });
  mods = await import("../../src/admin_assets/js/connect.js");
  mods.menu = await import("../../src/admin_assets/js/menu.js");
});

describe("connect snippets carry the /mcp/ domain prefix (docs/24 P3)", () => {
  it("endpointUrl builds origin + /mcp/ + name", () => {
    expect(mods.endpointUrl("redis")).toBe("http://127.0.0.1:19999/mcp/redis");
    expect(mods.endpointUrl("deep-wiki")).toBe("http://127.0.0.1:19999/mcp/deep-wiki");
  });

  it("the claude add command carries the prefixed URL", () => {
    const s = mods.claudeSnippet("redis", "sekret");
    expect(s).toContain(" http://127.0.0.1:19999/mcp/redis ");
    expect(s).not.toContain(" 19999/redis");
    expect(s).toContain("Bearer sekret");
  });

  it("the codex config block carries the prefixed URL", () => {
    const s = mods.codexSnippet("redis", "sekret");
    expect(s).toContain('url = "http://127.0.0.1:19999/mcp/redis"');
  });

  it("the .mcp.json entry carries the prefixed URL", () => {
    const s = mods.mcpJsonSnippet("redis", "sekret");
    const parsed = JSON.parse(s);
    expect(parsed.mcpServers.redis.url).toBe("http://127.0.0.1:19999/mcp/redis");
    expect(parsed.mcpServers.redis.headers.Authorization).toBe("Bearer sekret");
  });

  it("the sidebar tooltip names the /mcp/ path, not the retired root shape", () => {
    const tip = mods.menu.tooltipOf({ name: "redis", type: "redis", source: "config", state: "up" });
    expect(tip.startsWith("/mcp/redis")).toBe(true);
    expect(tip).toContain("redis  ·  config  ·  up");
  });
});