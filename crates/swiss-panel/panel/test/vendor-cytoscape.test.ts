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

/* The vendored Cytoscape.js (SPEC §panel.vendor): the bytes are the stock npm 3.34.3
   dist/cytoscape.min.js, the UMD assigns the global the shim reads, and the license ships
   beside it. The memory page's tests draw on a stand-in engine (graph-fake.ts) - this file is
   where the real bundle is held to its contract, without a browser. */
import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import vm from "node:vm";

const DIR = "../../src/admin_assets/js/vendor/cytoscape/";
const read = (rel: string): Buffer => readFileSync(fileURLToPath(new URL(DIR + rel, import.meta.url)));

describe("vendored cytoscape 3.34.3", () => {
  /** The SHA-256 of the npm tarball's dist/cytoscape.min.js at vendoring time. A different hash
   *  is an upstream re-release under the same version or a local edit - both need eyes. */
  it("is the stock npm build, byte for byte", () => {
    const sha = createHash("sha256").update(read("3.34.3/cytoscape.min.js")).digest("hex").toUpperCase();
    expect(sha).toBe("5F3B5B529546D5AF1FC5628590AF033B74511A5B6F789F5F4682845863228B91");
  });

  it("run as a classic script, it assigns the factory the shim reads off the window", () => {
    // A <script> tag's semantics: top-level this, self, window and globalThis are one global.
    const sandbox: Record<string, unknown> = {};
    sandbox.self = sandbox;
    sandbox.window = sandbox;
    vm.createContext(sandbox);
    vm.runInContext(read("3.34.3/cytoscape.min.js").toString("utf8"), sandbox);
    expect(typeof sandbox.cytoscape).toBe("function");
    // Headless (no container) is enough to prove the factory is the factory. The options are
    // built inside the context: cytoscape takes only a plain object of its own realm's Object.
    const nodes: unknown = vm.runInContext(
      "(function () { var cy = cytoscape({ headless: true, elements: [{ group: 'nodes', data: { id: 'a' } }] });" +
      " var n = cy.nodes().length; cy.destroy(); return n; })()", sandbox);
    expect(nodes).toBe(1);
    expect(read("3.34.3/index.js").toString("utf8")).toContain("window.cytoscape");
  });

  it("ships its MIT license beside the version directory", () => {
    const license = read("LICENSE").toString("utf8");
    expect(license).toContain("The Cytoscape Consortium");
    expect(license).toContain("Permission is hereby granted, free of charge");
  });
});
