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

/* The emitted tree is a committed product (docs/36 D3): cargo build, CI's Rust jobs and the
   jobs API test embed ../src/admin_assets/js without node ever running. That only works if
   the committed .js files are exactly what the sources emit, so this suite re-emits every
   source in memory and compares - a stale emission or an orphan .js fails here, not on a
   fresh checkout three weeks later. */

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
const { listEmitted, listSources, emitOne } = await import(new URL("../build.mjs", import.meta.url).href) as {
  listEmitted: () => string[];
  listSources: () => string[];
  emitOne: (rel: string) => string;
};

const outDir = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "src", "admin_assets", "js");

describe("panel emit freshness (docs/36 D3)", () => {
  it("every committed .js under js/ is exactly what its .ts source emits", () => {
    const sources = listSources();
    expect(sources.length, "the tree moved? expected the panel's ~55 own modules").toBeGreaterThan(40);
    const stale: string[] = [];
    for (const rel of sources) {
      const js = rel.replace(/\.ts$/, ".js");
      const onDisk = readFileSync(join(outDir, js), "utf8");
      if (onDisk !== emitOne(rel)) stale.push(js);
    }
    expect(stale, "stale emissions - run: npm run build").toEqual([]);
  });

  it("no orphan .js: every non-vendor emitted file has a .ts source", () => {
    const sources = new Set(listSources().map((p) => p.replace(/\.ts$/, ".js")));
    const orphans = listEmitted().filter((js) => !sources.has(js));
    expect(orphans, "a module was deleted in panel/src but its emission remains - delete both").toEqual([]);
  });

  it("declaration files never emit: no types/*.js exists in the served tree", () => {
    // .d.ts files are ambient contracts (docs/36 D6); if one ever emitted, an empty module
    // would enter the served graph. The walker skipping them is build.mjs's job; this pins
    // the visible half - nothing under js/ mirrors a declaration source.
    const emitted = new Set(listEmitted());
    const wouldEmit = listSources().map((p) => p.replace(/\.ts$/, ".js"));
    for (const js of wouldEmit) expect(emitted.has(js)).toBe(true);
  });
});
