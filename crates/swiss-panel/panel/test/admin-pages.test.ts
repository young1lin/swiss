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

import { describe, expect, it } from "vitest";
import { createPageRegistry, groupPages } from "../src/page-core.js";

/** The registry is pure data + loaders, so it is unit-testable without a DOM. These pin the
 *  contracts the shell relies on: shape validation, duplicate refusal, order sorting, stale
 *  cache eviction when an entry path changes, and one retry after a failed dynamic import. */
describe("admin page registry", () => {
  const good = { id: "jobs", pluginId: "jobs", label: "Jobs", order: 10, path: "#jobs", entry: "/admin/js/views/jobs.js" };

  it("validates ids, labels and local entries", () => {
    const reg = createPageRegistry(async () => ({}));
    expect(() => reg.replace([{ ...good, id: "../evil" }])).toThrow();
    expect(() => reg.replace([{ ...good, label: "" }])).toThrow();
    expect(() => reg.replace([{ ...good, entry: "https://cdn.example/x.js" }])).toThrow();
    expect(() => reg.replace([{ ...good, entry: "/admin/js/../../secret.js" }])).toThrow();
    expect(() => reg.replace([good, { ...good }])).toThrow(/Duplicate/);
    reg.replace([{ ...good, order: "5" as unknown as number }]);
    expect(reg.list()[0].order).toBe(0); // a non-numeric order cannot NaN-poison the sort; it sorts first
  });

  it("sorts by order and keeps replacement atomic", () => {
    const reg = createPageRegistry(async () => ({}));
    reg.replace([good, { ...good, id: "aa", order: 1 }, { ...good, id: "zz", order: 2 }]);
    expect(reg.list().map((p: { id: string }) => p.id)).toEqual(["aa", "zz", "jobs"]);
    reg.replace([]);
    expect(reg.list()).toEqual([]);
    expect(reg.get("jobs")).toBeUndefined();
  });

  it("caches a loaded module per entry and retries after a failure", async () => {
    let loads = 0;
    const importer = async (entry: string) => {
      loads++;
      if (loads === 1) throw new Error("flaky");
      return { entry, mounted: true };
    };
    const reg = createPageRegistry(importer);
    reg.replace([good]);
    await expect(reg.load("jobs")).rejects.toThrow("flaky"); // first attempt fails
    const retried = await reg.load("jobs"); // the failed promise was evicted, so this retries
    expect(loads).toBe(2);
    expect(await reg.load("jobs")).toBe(retried); // then served from cache
    expect(loads).toBe(2);
    // A changed entry path invalidates the cache for that page only.
    reg.replace([{ ...good, entry: "/admin/plugins/jobs.js" }]);
    const fresh = await reg.load("jobs");
    expect(loads).toBe(3);
    expect(fresh).not.toBe(retried);
    await expect(reg.load("nope")).rejects.toThrow(/Unknown page/);
  });
});

/** The two-level navigation is rendered from grouped pages, so the grouping itself is pinned here:
 *  group by pluginId, order groups by the smallest page order they hold, and never drop a page
 *  whose plugin row is missing. Pure data in, pure data out — no DOM anywhere in this block. */
describe("page grouping", () => {
  const page = (id: string, pluginId: string, order: number, label?: string) =>
    ({ id, pluginId, label: label ?? id.toUpperCase(), order, path: "#" + id, entry: "/admin/js/views/" + id + ".js" });

  it("merges pages that share a pluginId into one group, pages sorted by order inside it", () => {
    const groups = groupPages(
      [page("traffic", "mcp", 20), page("mcps", "mcp", 10), page("tunnels", "tunnels", 30)],
      [{ id: "mcp", label: "MCP" }, { id: "tunnels", label: "Tunnels" }],
      {},
    );
    expect(groups.map((g: { id: string }) => g.id)).toEqual(["mcp", "tunnels"]);
    expect(groups[0].label).toBe("MCP");
    expect(groups[0].pages.map((p: { id: string }) => p.id)).toEqual(["mcps", "traffic"]);
    expect(groups[1].pages.map((p: { id: string }) => p.id)).toEqual(["tunnels"]);
  });

  it("orders groups by their smallest page order, so one low-order page promotes its whole group", () => {
    // mcp holds orders 1 and 10: the group's order is the MIN (1), which puts it ahead of jobs (50).
    const groups = groupPages(
      [page("jobs", "jobs", 50), page("traffic", "mcp", 1), page("mcps", "mcp", 10)],
      [{ id: "jobs", label: "Jobs" }, { id: "mcp", label: "MCP" }],
      {},
    );
    expect(groups.map((g: { id: string }) => g.id)).toEqual(["mcp", "jobs"]);
    expect(groups[0].order).toBe(1);
  });

  it("labels a group from the plugin row, then fallbackLabels, then the page itself — never dropping it", () => {
    // pluginId "host" has no inventory row (the management page is synthesized client-side).
    const withFallback = groupPages([page("plugins", "host", 1000)], [], { host: "Settings" });
    expect(withFallback).toHaveLength(1);
    expect(withFallback[0].label).toBe("Settings");
    expect(withFallback[0].pages.map((p: { id: string }) => p.id)).toEqual(["plugins"]);
    // No fallback either: the page's own label becomes the group label, and the page survives.
    const bare = groupPages([page("plugins", "host", 1000, "Plugins")], [], {});
    expect(bare).toHaveLength(1);
    expect(bare[0].label).toBe("Plugins");
    expect(bare[0].pages).toHaveLength(1);
  });

  it("groups with null plugins, the 404 path an older gateway serves", () => {
    const groups = groupPages(
      [page("mcps", "mcp", 10), page("traffic", "mcp", 20)],
      null,
      { mcp: "MCP" },
    );
    expect(groups).toHaveLength(1);
    expect(groups[0].label).toBe("MCP");
    expect(groups[0].pages.map((p: { id: string }) => p.id)).toEqual(["mcps", "traffic"]);
  });

  it("maps an empty page list to an empty group list", () => {
    expect(groupPages([], [{ id: "mcp", label: "MCP" }], {})).toEqual([]);
  });
});
