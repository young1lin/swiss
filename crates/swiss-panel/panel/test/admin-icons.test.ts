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
import { readFileSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { emptyHtml, icon } from "../src/util.js";

/* Visual refresh V2 (docs/18): one inline svg sprite in the shell replaces every unicode
   glyph icon — ↻ ☾ ☀ ⋯ × + › each had their own weight and baseline. These anchors pin the
   sprite's presence, the ban on the old entities, and icon()'s contract. */

const admin = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets");

/* The sources are TypeScript under panel/src now (docs/36); .d.ts files carry no markup and
   are skipped. The emitted js tree is proven equivalent by panel-emit.test.ts. */
function walkSrc(dir: string): string[] {
  const out: string[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, entry.name);
    if (entry.isDirectory()) out.push(...walkSrc(p));
    else if (entry.name.endsWith(".ts") && !entry.name.endsWith(".d.ts")) out.push(p);
  }
  return out;
}

const shell = readFileSync(join(admin, "index.html"), "utf8");
const modules = walkSrc(join(dirname(fileURLToPath(import.meta.url)), "..", "src")).map((p) => readFileSync(p, "utf8"));

describe("visual refresh V2 — the sprite replaces unicode glyphs", () => {
  it("the shell carries the sprite with the icons the chrome needs", () => {
    for (const id of ["i-sun", "i-moon", "i-plus", "i-folder-plus", "i-ellipsis", "i-chevron-right", "i-chevron-left", "i-x", "i-expand", "i-collapse", "i-puzzle"]) {
      expect(shell, `symbol ${id}`).toContain(`<symbol id="${id}"`);
    }
    expect(shell).toContain("<svg hidden");
  });

  it("the toolbar refresh button is gone - the memory chip is a control, not a refresh button", () => {
    // Every view already reloads on the 6s poll; a dedicated button answered a question the
    // poll had already answered. Its one unique job - force-reloading Data, whose poll
    // leaves paged-in lists alone - lives on the r key; the memory chip's click re-reads
    // memory only (a reading is not a reload button).
    expect(shell).not.toContain('id="refreshBtn"');
    expect(shell).not.toContain("i-refresh-cw"); // the sprite entry goes with the button
    expect(shell).toContain('id="memChip"');
    // Focus mode (js/immersive.js) is the page-sized answer: the context bar's far right
    // carries the expand control, painted with its own two-state sprite icons (docs/13 D5
    // rev. — one shell-owned control, the same in-flow slot in normal and focus modes).
    expect(shell).toContain('id="expandBtn"');
  });

  it("the toolbar token button is gone - token management lives under the MCP group", () => {
    // The token exists FOR MCP clients, so the panel manages it where that story is told:
    // the MCP group's Token page. The toolbar's right side keeps only Appearance, and the
    // key icon went with the button (nothing else uses it).
    expect(shell).not.toContain('id="tokenBtn"');
    expect(shell).not.toContain("i-key"); // the sprite entry goes with the button
  });

  it("the unicode icon entities are gone from the shell and every module", () => {
    for (const entity of ["&#8635;", "&#9790;", "&#9728;", "&#9681;", "&#8943;", "&#10005;", "&#8250;", "\u203a", "\u2039", "\u00b7\u00b7\u00b7"]) {
      expect(shell, entity).not.toContain(entity);
      for (const src of modules) expect(src, entity).not.toContain(entity);
    }
  });

  it("icon() points at the sprite, hidden by default, labelled on request", () => {
    expect(icon("key")).toContain('href="#i-key"');
    expect(icon("key")).toContain('aria-hidden="true"');
    expect(icon("key", "Token")).toContain('aria-label="Token"');
    expect(icon("key", "Token")).not.toContain("aria-hidden");
  });
});

/* Visual refresh V7 (docs/18): one empty-state template — icon, title, hint, optional ghost
   action — used by the MCP pane, Jobs, Tunnels, Plugins and Data. Terminal keeps its own
   (it lives in the black frame with its own token system). */
describe("visual refresh V7 — one empty-state template", () => {
  it("emptyHtml() points at the sprite and keeps the title an h2", () => {
    const html = emptyHtml({ icon: "mcp", title: "Select an MCP" });
    expect(html).toContain('href="#i-mcp"');
    expect(html).toContain("<h2>Select an MCP</h2>");
  });

  it("carries the action as data-empty-action; the hint is optional", () => {
    const html = emptyHtml({ icon: "clock", title: "No jobs yet", hint: "Scheduled commands run locally.", action: "New" });
    expect(html).toContain('data-empty-action="New"');
    expect(html).toContain("Scheduled commands run locally.");
  });

  it("without an action there is no button", () => {
    expect(emptyHtml({ icon: "plug", title: "No SSH connections" })).not.toContain("data-empty-action");
  });
});

/* The MCP plugin seat wears the official MCP mark (modelcontextprotocol.io): the site logo
   mark — three interleaved hooks — mapped from the logo's own grid onto the 24-grid. The
   generic i-server box it replaced left the sprite with its last consumer. */
describe("the MCP glyph is the official mark", () => {
  it("the sprite carries i-mcp; the retired server box is gone", () => {
    expect(shell).toContain('<symbol id="i-mcp"');
    expect(shell).not.toContain("i-server");
  });
});
