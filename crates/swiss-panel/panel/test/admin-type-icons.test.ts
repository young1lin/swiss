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

import { beforeAll, describe, expect, it } from "vitest";

/* docs/29 — the launch-tag glyph map. docs/37 R5: typeTagNode() BUILDS the glyph, so the
   suite reads the attributes off the returned svg (mapped tags) or the plain string the
   unmapped fallback hands back. What is pinned here is the whitelist, the fallback, and
   the aria label that keeps the word audible when the chip goes graphical. */
let util: typeof import("../src/util.js");

beforeAll(async () => {
  const anyG = globalThis as unknown as Record<string, unknown>;
  if (!anyG.document) {
    const stubSvg = () => {
      return {
        attrs: {} as Record<string, string>,
        kids: [] as unknown[],
        setAttribute(k: string, v: string) { this.attrs[k] = v; },
        appendChild(n: unknown) { this.kids.push(n); return n; },
      };
    };
    anyG.document = { createElementNS: () => stubSvg() };
  }
  util = await import("../src/util.js");
});

const hrefOf = (n: unknown) => (n as { kids: { attrs: Record<string, string> }[] }).kids[0].attrs.href;
const attr = (n: unknown, k: string) => (n as { attrs: Record<string, string> }).attrs[k];

describe("docs/29: typeTagNode — glyph when mapped, word when not", () => {
  it("a mapped tag renders its sprite glyph and carries the word as the aria-label", () => {
    const svg = util.typeTagNode("mysql");
    expect(hrefOf(svg)).toBe("#i-mysql");
    expect(attr(svg, "role")).toBe("img");
    expect(attr(svg, "aria-label")).toBe("mysql");
  });

  it("the proc launch words and the in-process drivers map to their marks", () => {
    const marks: Record<string, string> = {
      uvx: "#i-package", npx: "#i-package", docker: "#i-docker", mariadb: "#i-mariadb",
      redis: "#i-redis", pg: "#i-pg", postgres: "#i-pg", http: "#i-globe", rest: "#i-plug",
      figma: "#i-figma", "zai-vision": "#i-zai",
    };
    for (const tag of Object.keys(marks)) expect(hrefOf(util.typeTagNode(tag)), tag).toBe(marks[tag]);
  });

  it("an unmapped tag falls back to the plain word — the chip users had before", () => {
    // docs/37 R5: the fallback is a TEXT NODE now, so the tag string is the string itself;
    // a tag that looks like markup can never be parsed as markup.
    expect(util.typeTagNode("echo")).toBe("echo");
    expect(util.typeTagNode("node")).toBe("node");
    expect(util.typeTagNode("<script>")).toBe("<script>");
  });
});
