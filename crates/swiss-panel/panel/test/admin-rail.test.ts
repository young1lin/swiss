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
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  defaultPinIds,
  glyphNode,
  paletteRows,
  pinFull,
  pinnedGroups,
  pluginGlyph,
  RAIL_LIMIT,
} from "../src/plugin-palette.js";

/* The rail/palette model (SPEC §panel.nav): the rail is the pinned SHORTLIST of plugin groups,
 * the palette is the whole list, searchable. Everything here is pure — the DOM half of
 * plugin-palette.js is one overlay, the policy is data. The groups are the shape
 * page-core.groupPages produces: { id, label, order, pages }. */
const groups = [
  { id: "mcp", label: "MCP", order: 10, pages: [{ id: "mcps" }] },
  { id: "tunnels", label: "Tunnels", order: 30, pages: [{ id: "tunnels" }] },
  { id: "terminal", label: "Terminal", order: 70, pages: [{ id: "terminal" }] },
  { id: "host", label: "Settings", order: 1000, pages: [{ id: "plugins" }, { id: "secrets" }] },
] as PageGroup[];

/* The nine groups today's host serves, in served group order (SPEC §panel.nav: group order is
 * the smallest page order in the group; the panel synthesizes host at 1000). The process
 * plugin contributes no page and never forms a group. */
const nineGroups = [
  { id: "mcp", label: "MCP", order: 10, pages: [{ id: "mcps" }] },
  { id: "tunnels", label: "Tunnels", order: 30, pages: [{ id: "tunnels" }] },
  { id: "data", label: "Data", order: 40, pages: [{ id: "data" }] },
  { id: "jobs", label: "Jobs", order: 50, pages: [{ id: "jobs" }] },
  { id: "terminal", label: "Terminal", order: 70, pages: [{ id: "terminal" }] },
  { id: "remote", label: "Remote", order: 75, pages: [{ id: "remote" }] },
  { id: "memory", label: "Memory", order: 80, pages: [{ id: "memory" }] },
  { id: "llm", label: "LLM", order: 85, pages: [{ id: "llm-providers" }] },
  { id: "host", label: "Settings", order: 1000, pages: [{ id: "plugins" }, { id: "secrets" }] },
] as PageGroup[];

/* A host grown past the rail: n groups g01.., orders stepped by 10 so group order is id order. */
function makeGroups(n: number): PageGroup[] {
  return Array.from({ length: n }, (_, i) => {
    const id = "g" + String(i + 1).padStart(2, "0");
    return { id, label: id.toUpperCase(), order: (i + 1) * 10, pages: [{ id }] } as PageGroup;
  });
}

describe("the rail shortlist", () => {
  it("defaults to every module - today's nine groups all seated, LLM and Settings included", () => {
    expect(RAIL_LIMIT).toBe(15);
    expect(defaultPinIds(nineGroups)).toEqual(nineGroups.map((g) => { return g.id; }));
    expect(pinnedGroups(nineGroups, null).map((g) => { return g.id; })).toEqual(nineGroups.map((g) => { return g.id; }));
    expect(defaultPinIds([])).toEqual([]);
  });

  it("past the cap the default keeps the first fifteen seats, in group order", () => {
    expect(defaultPinIds(makeGroups(17))).toEqual(makeGroups(15).map((g) => { return g.id; }));
    expect(pinnedGroups(makeGroups(17), null)).toEqual(makeGroups(15));
  });

  it("exactly fifteen groups: every seat shows", () => {
    expect(pinnedGroups(makeGroups(15), null)).toHaveLength(15);
  });

  it("a saved list longer than the cap is sliced to the first fifteen, in group order", () => {
    const saved = makeGroups(17).map((g) => { return g.id; });
    expect(pinnedGroups(makeGroups(17), saved)).toEqual(makeGroups(15));
  });

  it("the star stops offering seats once the rail is full", () => {
    expect(pinFull(makeGroups(15).map((g) => { return g.id; }))).toBe(true);
    expect(pinFull(makeGroups(14).map((g) => { return g.id; }))).toBe(false);
    // Unpinning one frees the seat again.
    expect(pinFull(makeGroups(15).map((g) => { return g.id; }).slice(0, 14))).toBe(false);
  });
  it("a short host seats every group by default", () => {
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

  it("under default pins shows one section - Pinned - holding every module", () => {
    const rows = paletteRows(nineGroups, defaultPinIds(nineGroups), "");
    expect(rows).toHaveLength(1);
    expect(rows[0].section).toBe("Pinned");
    expect(rows[0].groups.map((g) => { return g.id; })).toEqual(nineGroups.map((g) => { return g.id; }));
  });

  it("searches by label and by id, case-insensitively", () => {
    // No pins means no Pinned section: everything lands under All plugins.
    expect(paletteRows(groups, [], "tun")[0].groups.map((g) => g.id)).toEqual(["tunnels"]);
    expect(paletteRows(groups, [], "MCP")[0].groups.map((g) => g.id)).toEqual(["mcp"]);
    expect(paletteRows(groups, [], "settings")[0].groups.map((g) => g.id)).toEqual(["host"]);
  });
});

describe("saved pin state", () => {
  /* pinnedGroups reads the store only when the caller passes no explicit list, so a stubbed
   * localStorage here leaves every other test untouched (the file's own globalThis-poke
   * precedent sits in the glyph tests below). */
  it("parses defensively and migrates: v1 arrays fall to the default, v2 lists are durable", () => {
    const anyG = globalThis as unknown as Record<string, unknown>;
    const keep = anyG.localStorage;
    const backing = new Map<string, string>();
    anyG.localStorage = {
      getItem: (k: string) => { return backing.has(k) ? backing.get(k) : null; },
      setItem: (k: string, v: string) => { void backing.set(k, v); },
    };
    try {
      // No key stored: the default seats every group.
      expect(pinnedGroups(nineGroups).map((g) => { return g.id; })).toEqual(nineGroups.map((g) => { return g.id; }));
      // Garbage JSON: the same fallback, never a throw.
      backing.set("swiss.rail.pinned", "{oops");
      expect(pinnedGroups(nineGroups).map((g) => { return g.id; })).toEqual(nineGroups.map((g) => { return g.id; }));
      // A v1 bare array is the pre-fifteen era's frozen default, whatever it holds:
      // it reads as never-set, so the raise reaches browsers that saved under seven.
      backing.set("swiss.rail.pinned", JSON.stringify(["mcp", 5, null, "host"]));
      expect(pinnedGroups(nineGroups).map((g) => { return g.id; })).toEqual(nineGroups.map((g) => { return g.id; }));
      backing.set("swiss.rail.pinned", "[]");
      expect(pinnedGroups(nineGroups).map((g) => { return g.id; })).toEqual(nineGroups.map((g) => { return g.id; }));
      // v2 keeps its exact list: non-string entries drop, the rest keep their seats.
      backing.set("swiss.rail.pinned", JSON.stringify({ v: 2, ids: ["mcp", 5, null, "host"] }));
      expect(pinnedGroups(nineGroups).map((g) => { return g.id; })).toEqual(["mcp", "host"]);
      // A deliberately emptied v2 list stays empty: the rail is the ... seat alone.
      backing.set("swiss.rail.pinned", JSON.stringify({ v: 2, ids: [] }));
      expect(pinnedGroups(nineGroups)).toEqual([]);
      // An unknown version is not this store's to interpret: the default, never a guess.
      backing.set("swiss.rail.pinned", JSON.stringify({ v: 3, ids: ["mcp"] }));
      expect(pinnedGroups(nineGroups).map((g) => { return g.id; })).toEqual(nineGroups.map((g) => { return g.id; }));
    } finally {
      anyG.localStorage = keep;
    }
  });
});

describe("seat glyphs", () => {
  // SPEC §panel.toolchain: the glyph is a BUILT node now, so the assertions read its attributes off
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
    // Memory wears a brain, drawn in the shell's sprite.
    expect(hrefOf(glyphNode({ id: "memory", label: "Memory" } as PageGroup))).toBe("#i-brain");
    const shell = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "index.html"), "utf8");
    expect(shell).toContain('<symbol id="i-brain"');
    // The llm plugin - the models memory and later consumers ask - wears sparkles.
    expect(hrefOf(glyphNode({ id: "llm", label: "LLM" } as PageGroup))).toBe("#i-sparkles");
    expect(shell).toContain('<symbol id="i-sparkles"');
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
