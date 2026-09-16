import { beforeAll, describe, expect, it } from "vitest";

/* docs/29 — the launch-tag glyph map. Pure string building (icon()/esc() from util.js), so no
   DOM is needed: what is pinned here is the whitelist, the fallback, and the aria label that
   keeps the word audible when the chip goes graphical. */
let util: typeof import("../../src/admin_assets/js/util.js");

beforeAll(async () => {
  util = await import("../../src/admin_assets/js/util.js");
});

describe("docs/29: typeTagHtml — glyph when mapped, word when not", () => {
  it("a mapped tag renders its sprite glyph and carries the word as the aria-label", () => {
    const html = util.typeTagHtml("mysql");
    expect(html).toContain('href="#i-mysql"');
    expect(html).toContain('role="img"');
    expect(html).toContain('aria-label="mysql"');
  });

  it("the proc launch words and the in-process drivers map to their marks", () => {
    expect(util.typeTagHtml("uvx")).toContain("#i-package");
    expect(util.typeTagHtml("npx")).toContain("#i-package");
    expect(util.typeTagHtml("docker")).toContain("#i-docker");
    expect(util.typeTagHtml("mariadb")).toContain("#i-mariadb");
    expect(util.typeTagHtml("redis")).toContain("#i-redis");
    expect(util.typeTagHtml("pg")).toContain("#i-pg");
    expect(util.typeTagHtml("postgres")).toContain("#i-pg");
    expect(util.typeTagHtml("http")).toContain("#i-globe");
    expect(util.typeTagHtml("rest")).toContain("#i-plug");
    expect(util.typeTagHtml("figma")).toContain("#i-figma");
    expect(util.typeTagHtml("zai-vision")).toContain("#i-zai");
  });

  it("an unmapped tag falls back to the escaped word — the chip users had before", () => {
    expect(util.typeTagHtml("echo")).toBe("echo");
    expect(util.typeTagHtml("node")).toBe("node");
    expect(util.typeTagHtml("<script>")).toBe("&lt;script&gt;");
  });
});
