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
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

/* The one-shot mcp_gateway_* -> swiss.* localStorage migration was removed with the
   rename's pairing window: an outside user never had the old keys, and this browser's
   one move happened long ago. What stays pinned is that the old prefix never comes
   back — no stray literal in the sources may resurrect a key nobody writes anymore. */

const srcDir = join(dirname(fileURLToPath(import.meta.url)), "..", "src");

function sourceFiles(dir: string): string[] {
  const out: string[] = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (name.endsWith(".ts")) out.push(p);
  }
  return out;
}

describe("the retired localStorage prefix", () => {
  it("no source mentions mcp_gateway_ anywhere any more", () => {
    const offenders: string[] = [];
    for (const p of sourceFiles(srcDir)) {
      const code = readFileSync(p, "utf8");
      if (code.includes("mcp_gateway_")) offenders.push(p);
    }
    expect(offenders).toEqual([]);
  });
});
