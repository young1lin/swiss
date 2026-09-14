import { describe, it, expect } from "vitest";
import {
  defaultPinIds,
  glyphHtml,
  paletteRows,
  pinnedGroups,
  pluginGlyph,
  RAIL_LIMIT,
} from "../../src/admin_assets/js/plugin-palette.js";

/* The rail/palette model (docs/13 D5): the rail is the pinned SHORTLIST of plugin groups,
 * the palette is the whole list, searchable. Everything here is pure — the DOM half of
 * plugin-palette.js is one overlay, the policy is data. The groups are the shape
 * page-core.groupPages produces: { id, label, order, pages }. */
const groups = [
  { id: "mcp", label: "MCP", order: 10, pages: [{ id: "mcps" }] },
  { id: "tunnels", label: "Tunnels", order: 30, pages: [{ id: "tunnels" }] },
  { id: "terminal", label: "Terminal", order: 70, pages: [{ id: "terminal" }] },
  { id: "host", label: "Gateway", order: 1000, pages: [{ id: "plugins" }, { id: "secrets" }] },
];

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
    expect(paletteRows(groups, [], "gateway")[0].groups.map((g) => g.id)).toEqual(["host"]);
  });
});

describe("seat glyphs", () => {
  it("built-ins use the sprite; unknown plugins fall back to their initial", () => {
    expect(pluginGlyph({ id: "mcp", label: "MCP" })).toBe("server");
    expect(glyphHtml({ id: "mcp", label: "MCP" })).toContain("#i-server");
    expect(pluginGlyph({ id: "kubernetes", label: "Kubernetes" })).toBe(null);
    expect(glyphHtml({ id: "kubernetes", label: "Kubernetes" })).toContain('class="rail-glyph"');
    expect(glyphHtml({ id: "kubernetes", label: "Kubernetes" })).toContain(">K</span>");
  });
});
