import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { requiresBadge, rowHtml } from "../../src/admin_assets/js/views/plugins.js";

/** The dependency badge is the panel's half of the W3 contract (docs/12): the inventory
 *  states a plugin's required capabilities with a met/unmet verdict, and the row must SAY
 *  when the floor is missing — "needs connection-catalog (no provider)" — so disabling the
 *  provider reads as a consequence, not as a mysteriously broken Data view. The helper is
 *  pure row-JSON-in / badge-HTML-out; no DOM involved. */
describe("plugins view dependency badge", () => {
  it("names the capability when the requirement is unmet", () => {
    expect(requiresBadge({ id: "data", requires: ["connection-catalog"], requiresMet: false }))
      .toBe('· needs connection-catalog <span class="warn">(no provider)</span>');
  });

  it("stays silent while the requirement is met", () => {
    expect(requiresBadge({ id: "data", requires: ["connection-catalog"], requiresMet: true })).toBe("");
  });

  it("has nothing to say for plugins without requirements", () => {
    // Plugins that need nothing carry no requires keys at all — absent, not null.
    expect(requiresBadge({ id: "mcp" })).toBe("");
    expect(requiresBadge({ id: "jobs", requires: [], requiresMet: true })).toBe("");
  });

  it("escapes capability names and joins several", () => {
    expect(requiresBadge({ id: "x", requires: ["a<b", "c&d"], requiresMet: false }))
      .toBe('· needs a&lt;b, c&amp;d <span class="warn">(no provider)</span>');
  });
});

/* Visual refresh V4 (docs/18): the plugins row is dot + name + one grey line, the toggle is
   a switch, and the version moves into the row title — the row says what it is, the dot says
   what it is doing. */
describe("visual refresh V4 — the plugins row", () => {
  it("toggles with a switch, not a Disable/Enable button", () => {
    const row = rowHtml({ id: "data", label: "Data", enabled: true, state: "active", pages: ["mcps", "traffic"], version: "0.1" });
    expect(row).toContain('role="switch"');
    expect(row).toContain('aria-checked="true"');
    expect(row).not.toContain(">Disable<");
    expect(row).not.toContain(">Enable<");
  });

  it("keeps id and pages on the grey line; version rides the title, state is the dot", () => {
    const row = rowHtml({ id: "data", label: "Data", enabled: true, state: "active", pages: ["mcps", "traffic"], version: "0.1" });
    expect(row).toContain("<code>data</code>");
    expect(row).toContain("pages: mcps, traffic");
    expect(row).toContain('title="v0.1"');
    expect(row).not.toContain("v0.1</span>");
    expect(row).not.toContain("· active");
  });

  it("a pageless plugin says so", () => {
    expect(rowHtml({ id: "host", label: "Gateway", enabled: true, state: "active" })).toContain("· no page");
  });

  it("the dot carries the host's own state word as its title (docs/18 V6)", () => {
    expect(rowHtml({ id: "data", label: "Data", enabled: true, state: "active" })).toContain('data-dot title="active"');
    expect(rowHtml({ id: "mcp", label: "MCP", enabled: false, state: "active" })).toContain('data-dot title="disabled"');
    expect(rowHtml({ id: "x", label: "X", enabled: true, state: "failed" })).toContain('data-dot title="failed"');
  });

  it("views.css stops centring pane content", () => {
    const css = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets", "styles", "views.css"), "utf8");
    expect(css).toContain("margin-inline: 0");
  });
});
