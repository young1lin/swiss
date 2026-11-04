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

import { readFileSync } from "node:fs";
import { cpus } from "node:os";
import { defineConfig } from "vitest/config";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

// No setup file on purpose: the old Node-repo setup pinned a master key and home for the
// server suite — panel tests need neither. Anything a DOM-stub test needs, it stubs itself.

/* The sources under panel/src import the vendored runtime ("./vendor/cronstrue/…" from
   jobs.ts, "../vendor/xterm/…" from views/terminal.ts) by the path the SERVED tree needs:
   emission writes them back beside js/vendor, where the browser resolves them. From the
   source tree that path does not exist, so this alias points vite at the real vendor files.
   tsc needs no alias: panel/src/vendor/<same-path>/index.d.ts mirrors the shapes (SPEC §panel.toolchain)
   and the specifier resolves there for type-checking. */
const vendorDir = resolve(fileURLToPath(new URL("../src/admin_assets/js/vendor", import.meta.url)));

/* Same problem, other direction: page descriptors carry their ENTRY as the served path
   ("/admin/js/views/traffic.js"), and page-registry's loader imports that string at
   runtime. From the test tree the path does not exist either, so the served /admin/js/
   prefix maps onto panel/src - the TypeScript twin of the line-for-line emit (SPEC §panel.toolchain).
   Without it, a suite that drives the real navigatePage (SPEC §panel.nav's seat test) cannot
   load a single view module; with it, the loaded module IS the source instance the suite
   imported directly. */
const srcDir = resolve(fileURLToPath(new URL("./src", import.meta.url)));

/* A vendored upstream ES module keeps its sourceMappingURL comment (byte-identical, SPEC
   §panel.vendor) but not the .map it names. Loaded by vite's own fs path, each run would warn
   "Failed to load source map"; loaded here, verbatim, vite takes the file as it is. */
const vendorVerbatim = {
  name: "vendor-verbatim",
  load(id: string): string | null {
    // vite ids are slash paths on every platform.
    return /\/admin_assets\/js\/vendor\/.*\.mjs$/.test(id) ? readFileSync(id, "utf8") : null;
  },
};

export default defineConfig({
  plugins: [vendorVerbatim],
  resolve: {
    alias: [
      // `../vendor/`, `./vendor/`, and the generated codecs' deeper `../../../../../vendor/`.
      { find: /^(?:\.\/|(?:\.\.\/)+)vendor\//, replacement: vendorDir + "/" },
      { find: /^\/admin\/js\//, replacement: srcDir + "/" },
    ],
  },
  // One worker per test file pegs every core of a small machine (the suite is over a
  // hundred files): cap workers at four or half the cores, never below two. A slower run is
  // free; a starved deadline is a false failure.
  test: {
    environment: "node",
    include: ["test/**/*.test.ts"],
    maxWorkers: Math.min(4, Math.max(2, Math.floor(cpus().length / 2))),
  },
});
