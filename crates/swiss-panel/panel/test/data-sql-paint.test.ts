// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const shell = readFileSync(join(here, "..", "..", "src", "admin_assets", "index.html"), "utf8");
document.body.innerHTML = Array.from(new Set(Array.from(shell.matchAll(/id="([a-zA-Z0-9_-]+)"/g)).map(m => m[1])))
  .map(id => '<div id="' + id + '"></div>').join("");
const { dbSqlPaint } = await import("../src/data-filters.js");

describe("the large SQL editor", () => {
  afterEach(() => { document.body.replaceChildren(); vi.unstubAllGlobals(); });

  it("preserves the complete text and colors without creating a span for every keyword", () => {
    const registry = new Map<string, Set<Range>>();
    vi.stubGlobal("CSS", { highlights: registry });
    vi.stubGlobal("Highlight", class extends Set<Range> {});
    document.body.innerHTML = '<textarea id="dbSql"></textarea><pre id="dbSqlHl"></pre>';
    const input = document.querySelector<HTMLTextAreaElement>("#dbSql")!;
    const painted = document.querySelector<HTMLElement>("#dbSqlHl")!;
    const sql = "SELECT id FROM orders WHERE id = 42; -- <keep & comment 😀>\n".repeat(1500);
    input.value = sql;
    dbSqlPaint();
    expect(painted.textContent).toBe(sql + "\n");
    expect(painted.querySelectorAll(".jv-w").length).toBe(0);
    expect(painted.querySelectorAll(".jv-c").length).toBe(1500);
    expect(painted.querySelectorAll(".jv-n").length).toBe(1500);
    expect(registry.get("swiss-db-sql-jv-w")?.size).toBeGreaterThan(1500);
    expect(Array.from(registry.get("swiss-db-sql-jv-w") || [])[0].toString()).toBe("SELECT");
    expect(painted.querySelector("keep")).toBeNull();
    input.value = "SELECT 1";
    dbSqlPaint();
    expect(painted.textContent).toBe("SELECT 1\n");
    expect(painted.querySelector(".jv-w")?.textContent).toBe("SELECT");
    expect(registry.size).toBe(0);
  });

  it("falls back to ordinary highlighted spans when native ranges are unavailable", () => {
    vi.stubGlobal("CSS", undefined);
    document.body.innerHTML = '<textarea id="dbSql"></textarea><pre id="dbSqlHl"></pre>';
    const input = document.querySelector<HTMLTextAreaElement>("#dbSql")!;
    const painted = document.querySelector<HTMLElement>("#dbSqlHl")!;
    input.value = "SELECT 1;\n".repeat(6500);
    dbSqlPaint();
    expect(painted.textContent).toBe(input.value + "\n");
    expect(painted.querySelector(".jv-w")?.textContent).toBe("SELECT");
    expect(painted.querySelector(".jv-n")?.textContent).toBe("1");
  });
});
