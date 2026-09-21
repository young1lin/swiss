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

import type { PageGroup } from "../src/types/dom.js";
import { describe, it, expect } from "vitest";
import {
  defaultPinIds,
  glyphNode,
  paletteRows,
  pinnedGroups,
  pluginGlyph,
  RAIL_LIMIT,
} from "../src/plugin-palette.js";

/* The rail/palette model (docs/13 D5): the rail is the pinned SHORTLIST of plugin groups,
 * the palette is the whole list, searchable. Everything here is pure — the DOM half of
 * plugin-palette.js is one overlay, the policy is data. The groups are the shape
 * page-core.groupPages produces: { id, label, order, pages }. */
const groups = [
  { id: "mcp", label: "MCP", order: 10, pages: [{ id: "mcps" }] },
  { id: "tunnels", label: "Tunnels", order: 30, pages: [{ id: "tunnels" }] },
  { id: "terminal", label: "Terminal", order: 70, pages: [{ id: "terminal" }] },
  { id: "host", label: "Settings", order: 1000, pages: [{ id: "plugins" }, { id: "secrets" }] },
] as PageGroup[];

describe("the rail shortlist", () => {
  it("defaults to the first RAIL_LIMIT groups in group order", () => {
    expect(RAIL_LIMIT).toBeGreaterThanOrEqual(5);
    expect(defaultPinIds(groups)).toEqual(["mcp", "tunnels", "terminal", "host"]);
    expect(pinnedGroups(groups, null).map((g) => g.id)).toEqual(["mcp", "tunnels", "terminal", "host"]);
  });

  it("pins select seats; they never reorder the rail", () => {
    expect(pinnedGroups(groups, ["host", "mcp"]).map((g) => g.id)).toEqual(["mcp", "host"]);
    // A pinned id the registry no longer holds is simply absent — stale pins cannot ghost.
    expect(pinnedGroups(groups, ["mcp", "gone"]).map((g) => g.id)).toEqual(["mcp"]);
  });
});

describe("the palette rows", () => {
  it("splits Pinned from All plugins and drops empty sections", () => {
    const rows = paletteRows(groups, ["terminal"], "");
    expect(rows.map((r) => r.section)).toEqual(["Pinned", "All plugins"]);
    expect(rows[0].groups.map((g) => g.id)).toEqual(["terminal"]);
    expect(rows[1].groups.map((g) => g.id)).toEqual(["mcp", "tunnels", "host"]);
    expect(paletteRows(groups, defaultPinIds(groups), "nomatch")).toEqual([]);
  });

  it("searches by label and by id, case-insensitively", () => {
    // No pins means no Pinned section: everything lands under All plugins.
    expect(paletteRows(groups, [], "tun")[0].groups.map((g) => g.id)).toEqual(["tunnels"]);
    expect(paletteRows(groups, [], "MCP")[0].groups.map((g) => g.id)).toEqual(["mcp"]);
    expect(paletteRows(groups, [], "settings")[0].groups.map((g) => g.id)).toEqual(["host"]);
  });
});

describe("seat glyphs", () => {
  // docs/37 R5: the glyph is a BUILT node now, so the assertions read its attributes off
  // the SVG the builder returns. The stub satisfies the two createElementNS calls the
  // builder makes (the svg wrapper and its use) - this suite has no DOM otherwise.
  const stubSvg = () => {
    return {
      attrs: {} as Record<string, string>,
      kids: [] as { attrs: Record<string, string> }[],
      setAttribute(k: string, v: string) { this.attrs[k] = v; },
      appendChild(n: { attrs: Record<string, string> }) { this.kids.push(n); return n; },
    };
  };
  const anyG = globalThis as unknown as Record<string, unknown>;
  if (!anyG.document) anyG.document = { createElementNS: () => stubSvg() };
  const hrefOf = (node: unknown) => (((node as unknown as { kids: { attrs: Record<string, string> }[] }).kids[0]).attrs.href);

  it("built-ins use the sprite, and the remote plugin wears its own mark", () => {
    expect(pluginGlyph({ id: "mcp", label: "MCP" } as PageGroup)).toBe("mcp");
    expect(hrefOf(glyphNode({ id: "mcp", label: "MCP" } as PageGroup))).toBe("#i-mcp");
    expect(pluginGlyph({ id: "remote", label: "Remote" } as PageGroup)).toBe("remote");
    expect(hrefOf(glyphNode({ id: "remote", label: "Remote" } as PageGroup))).toBe("#i-remote");
  });

  it("an unknown plugin falls back to the default puzzle glyph, never a letter", () => {
    // A seat is a row of drawn icons; an initial letter reads as a broken glyph (the
    // "Remote shows a bare R" sighting). The puzzle piece is the universal plugin mark.
    expect(pluginGlyph({ id: "kubernetes", label: "Kubernetes" } as PageGroup)).toBe(null);
    const svg = glyphNode({ id: "kubernetes", label: "Kubernetes" } as PageGroup);
    expect(hrefOf(svg)).toBe("#i-puzzle");
    // The fallback mark is still one sprite use - no letter, no per-plugin class.
    expect(((svg as unknown) as { kids: unknown[] }).kids.length).toBe(1);
  });
});
