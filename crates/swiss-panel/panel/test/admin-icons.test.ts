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

import { describe, expect, it } from "vitest";
import { readFileSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { emptyNode, iconNode } from "../src/util.js";

/* SPEC §panel.toolchain: the glyph and empty-state builders return NODES, so this file carries a
   minimal element stub (attrs + kids + writable props) and a serializer that spells the
   tree back out in tag form for the substring assertions below. */
class NodeStub {}
const mkNode = (tag: string): Record<string, any> => {
  return Object.assign(new NodeStub(), {
    tag, attrs: {} as Record<string, string>, kids: [] as unknown[], className: "",
    setAttribute(k: string, v: string) { this.attrs[k] = v; },
    appendChild(n: unknown) { this.kids.push(n); return n; },
  });
};
const anyG = globalThis as unknown as Record<string, unknown>;
if (!anyG.document) {
  anyG.document = {
    createElement: (t: string) => mkNode(t),
    createElementNS: (_ns: string, t: string) => mkNode(t),
    createDocumentFragment: () => mkNode("#frag"),
    createTextNode: (s: string) => ({ text: s }),
  };
}
if (!anyG.Node) anyG.Node = NodeStub;
const ser = (n: unknown): string => {
  const node = n as { text?: string; tag: string; attrs: Record<string, string>; className: string; kids: unknown[] };
  if (typeof node.text === "string") return node.text;
  const attrs = Object.keys(node.attrs || {}).map((k) => { return " " + k + '="' + node.attrs[k] + '"'; }).join("");
  const cls = node.className ? " class=\"" + node.className + "\"" : "";
  return "<" + node.tag + cls + attrs + ">" + (node.kids || []).map(ser).join("") + "</" + node.tag + ">";
};

/* Visual refresh V2 (SPEC §panel.design): one inline svg sprite in the shell replaces every unicode
   glyph icon — ↻ ☾ ☀ ⋯ × + › each had their own weight and baseline. These anchors pin the
   sprite's presence, the ban on the old entities, and icon()'s contract. */

const admin = join(dirname(fileURLToPath(import.meta.url)), "../..", "src", "admin_assets");

/* The sources are TypeScript under panel/src now (SPEC §panel.toolchain); .d.ts files carry no markup and
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
    // carries the expand control, painted with its own two-state sprite icons (SPEC §panel.nav
    // rev. — one shell-owned control, the same in-flow slot in normal and focus modes).
    expect(shell).toContain('id="expandBtn"');
  });

  it("the toolbar token button is gone - token management lives under the MCP group", () => {
    // The token exists FOR MCP clients, so the panel manages it where that story is told:
    // the MCP group's Token page. The toolbar's right side keeps only Appearance. i-key is
    // BACK in the sprite since SPEC §panel.design (the Data grid's PK column marker, not the
    // token button - which stays gone).
    expect(shell).not.toContain('id="tokenBtn"');
  });

  it("SPEC §panel.design: the V2 cleanup's symbols are in the sprite", () => {
    // The key marker, the remove/undo row controls, the picker's folder/file rows and its
    // Up button; i-x and i-history were already there for the chrome.
    for (const id of ["i-key", "i-undo", "i-arrow-up", "i-folder", "i-file", "i-history", "i-x"]) {
      expect(shell, `symbol ${id}`).toContain(`<symbol id="${id}"`);
    }
  });

  it("SPEC §panel.design: the retired unicode glyphs are gone from every module source", () => {
    // The acceptance grep as a pin: key marker, remove/undo, history, folder, file (and
    // the gear the plan found already retired). Locale dictionaries never carried these.
    for (const glyph of ["\u26bf", "\u2715", "\u21a9", "\u21ba", "\ud83d\udcc1", "\ud83d\udcc4", "\u2699"]) {
      for (const src of modules) expect(src, glyph).not.toContain(glyph);
    }
    expect(shell, "the shell too").not.toContain("\u26bf");
  });

  it("the unicode icon entities are gone from the shell and every module", () => {
    for (const entity of ["&#8635;", "&#9790;", "&#9728;", "&#9681;", "&#8943;", "&#10005;", "&#8250;", "\u203a", "\u2039", "\u00b7\u00b7\u00b7"]) {
      expect(shell, entity).not.toContain(entity);
      for (const src of modules) expect(src, entity).not.toContain(entity);
    }
  });

  it("iconNode() points at the sprite, hidden by default, labelled on request", () => {
    // SPEC §panel.toolchain: the glyph is a BUILT node - the assertions read the attributes the
    // builder set, against the element stub this file installs above.
    const hrefOf = (n: unknown) => ser((n as { kids: unknown[] }).kids[0]);
    const attr = (n: unknown, k: string) => (n as unknown as { attrs: Record<string, string> }).attrs[k];
    expect(hrefOf(iconNode("key"))).toContain('href="#i-key"');
    expect(attr(iconNode("key"), "aria-hidden")).toBe("true");
    expect(attr(iconNode("key", "Token"), "aria-label")).toBe("Token");
    expect(attr(iconNode("key", "Token"), "aria-hidden")).toBeUndefined();
  });
});

/* Visual refresh V7 (SPEC §panel.design): one empty-state template — icon, title, hint, optional ghost
   action — used by the MCP pane, Jobs, Tunnels, Plugins and Data. Terminal keeps its own
   (it lives in the black frame with its own token system). */
describe("visual refresh V7 — one empty-state template", () => {
  it("emptyNode() points at the sprite and keeps the title an h2", () => {
    const html = ser(emptyNode({ icon: "mcp", title: "Select an MCP" }));
    expect(html).toContain('href="#i-mcp"');
    expect(html).toContain("<h2>Select an MCP</h2>");
  });

  it("carries the action as data-empty-action; the hint is optional", () => {
    const html = ser(emptyNode({ icon: "clock", title: "No jobs yet", hint: "Scheduled commands run locally.", action: "New" }));
    expect(html).toContain('data-empty-action="New"');
    expect(html).toContain("Scheduled commands run locally.");
  });

  it("without an action there is no button", () => {
    expect(ser(emptyNode({ icon: "plug", title: "No SSH connections" }))).not.toContain("data-empty-action");
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
