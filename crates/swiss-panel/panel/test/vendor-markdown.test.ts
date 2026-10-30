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

/* SPEC §panel.vendor - the vendored markdown-it and mermaid, held to their contract without a
   browser: each file is the npm tarball's own (SHA-256 pinned), the shim hands over what upstream
   exports, and each license ships beside its version directory. ui/markdown.ts's own tests draw
   with the real markdown-it and a stand-in mermaid (the diagrams need a layout engine); this file
   is where the real mermaid bundle is run, as the classic script the shim injects. */
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import vm from "node:vm";
import type markdownitType from "../src/vendor/markdown-it/15.0.2/index.js";

const DIR = "../../src/admin_assets/js/vendor/";
// The served shim itself, by its served path - the module the panel's import resolves to.
const { default: markdownit } = await import(new URL(DIR + "markdown-it/15.0.2/index.js", import.meta.url).href) as
  { default: typeof markdownitType };
const read = (rel: string): Buffer => readFileSync(fileURLToPath(new URL(DIR + rel, import.meta.url)));
const sha = (rel: string): string => createHash("sha256").update(read(rel)).digest("hex").toUpperCase();

describe("vendored markdown-it 15.0.2", () => {
  /** The SHA-256 of the npm tarball's dist/browser/markdown-it.esm.min.mjs at vendoring time. A
   *  different hash is an upstream re-release under the same version or a local edit - both need eyes. */
  it("is the stock npm build, byte for byte", () => {
    expect(sha("markdown-it/15.0.2/markdown-it.esm.min.mjs")).toBe("85FEB50FD6CE1B7C49ACB02B0337EB622ECC4FC2ED4AE0DCBB8C084556D463B6");
  });

  it("is a self-contained ES module: no import of its own, the parser as its default export", () => {
    const src = read("markdown-it/15.0.2/markdown-it.esm.min.mjs").toString("utf8");
    expect(src).not.toMatch(/^\s*import\b|\bimport\s*\(|\bfrom\s*["'`]/m);
    expect(read("markdown-it/15.0.2/index.js").toString("utf8")).toContain('export { default } from "./markdown-it.esm.min.mjs";');
    expect(markdownit({ html: false }).render("# a *b*\n\n<i>c</i>")).toBe("<h1>a <em>b</em></h1>\n<p>&lt;i&gt;c&lt;/i&gt;</p>\n");
  });

  it("ships its MIT license beside the version directory", () => {
    const license = read("markdown-it/LICENSE").toString("utf8");
    expect(license).toContain("Vitaly Puzrin, Alex Kocharin");
    expect(license).toContain("Permission is hereby granted, free of charge");
  });
});

describe("vendored mermaid 12.1.0", () => {
  /** The SHA-256 of the @mermaid-js/tiny 12.1.0 tarball's dist/mermaid.tiny.js at vendoring time. */
  it("is the stock npm build, byte for byte", () => {
    expect(sha("mermaid/12.1.0/mermaid.tiny.js")).toBe("143C1B9345C45963FF429DA4BDCD9051316A43522EA9EDD7CCB7A87FF947A804");
  });

  /** Tiny, not the full build: the full mermaid.min.js bundles the Eclipse ELK layout engine
   *  (elkjs, EPL-2.0), copyleft the panel does not ship. */
  it("carries no Eclipse ELK code", () => {
    expect(read("mermaid/12.1.0/mermaid.tiny.js").toString("utf8")).not.toContain("org.eclipse.elk");
  });

  it("run as a classic script, it assigns the API the shim reads off the window, and its parser works", async () => {
    // A <script> tag's semantics: top-level this, self, window and globalThis are one global.
    const sandbox: Record<string, unknown> = {};
    sandbox.self = sandbox;
    sandbox.window = sandbox;
    vm.createContext(sandbox);
    vm.runInContext(read("mermaid/12.1.0/mermaid.tiny.js").toString("utf8"), sandbox);
    const api = sandbox.mermaid as { render: unknown; initialize: unknown; parse: (text: string) => Promise<unknown> };
    expect(typeof api.render).toBe("function");
    expect(typeof api.initialize).toBe("function");
    // Parsing needs no layout: a flowchart parses, prose does not.
    await expect(api.parse("flowchart TD\n  A --> B")).resolves.toMatchObject({ diagramType: "flowchart-v2" });
    await expect(api.parse("not a diagram")).rejects.toBeTruthy();
    await expect(api.parse("sequenceDiagram\n  A->>B: hi")).resolves.toMatchObject({ diagramType: "sequence" });
    // What tiny drops is refused, not half-drawn: the fence stays as code.
    await expect(api.parse("mindmap\n  root")).rejects.toBeTruthy();
    expect(read("mermaid/12.1.0/index.js").toString("utf8")).toContain("window.mermaid");
  }, 30_000);

  it("ships its MIT license beside the version directory", () => {
    const license = read("mermaid/LICENSE").toString("utf8");
    expect(license).toContain("Knut Sveidqvist");
    expect(license).toContain("Permission is hereby granted, free of charge");
  });
});
