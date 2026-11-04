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

/* @bufbuild/protobuf 2.16.0 is vendored byte-identical (SPEC §panel.vendor). The hash pin
   fails if a byte moves; the closure walk fails if a relative import points outside the set. */

import { createHash } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, normalize } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const dir = join(here, "..", "..", "src", "admin_assets", "js", "vendor", "protobuf", "2.16.0");
const dtsDir = join(here, "..", "src", "vendor", "protobuf", "2.16.0");

function pins(): Record<string, string> {
  const parsed: unknown = JSON.parse(readFileSync(new URL("./vendor-protobuf.sha256.json", import.meta.url), "utf8"));
  if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("vendor pin is not an object");
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(parsed)) {
    if (typeof value !== "string") throw new Error(key);
    out[key] = value.toUpperCase();
  }
  return out;
}

function files(root: string): string[] {
  const out: string[] = [];
  const walk = (rel: string) => {
    for (const entry of readdirSync(join(root, rel), { withFileTypes: true })) {
      const next = rel ? rel + "/" + entry.name : entry.name;
      if (entry.isDirectory()) walk(next);
      else out.push(next);
    }
  };
  walk("");
  return out.sort();
}

describe("@bufbuild/protobuf 2.16.0 vendor pin (SPEC §panel.vendor)", () => {
  it("every vendored runtime file matches its recorded SHA-256", () => {
    const present = files(dir);
    const recorded = pins();
    expect(present.filter((name) => name.endsWith(".js")).sort()).toEqual(Object.keys(recorded).sort());
    for (const [rel, hex] of Object.entries(recorded)) {
      const digest = createHash("sha256").update(readFileSync(join(dir, rel))).digest("hex").toUpperCase();
      expect(digest, rel).toBe(hex);
    }
  });

  it("the vendored set is closed under its relative imports", () => {
    const present = new Set(files(dir));
    const declarations = new Set(files(dtsDir));
    const importRe = /from\s+["'](\.[^"']+)["']/g;
    const check = (root: string, rel: string, js: boolean) => {
      const text = readFileSync(join(root, rel), "utf8");
      for (const match of text.matchAll(importRe)) {
        let next = normalize(join(dirname(rel), match[1])).replaceAll("\\", "/");
        if (!next.endsWith(".js")) next += ".js";
        expect(present.has(next), rel + " imports " + next).toBe(true);
        if (!js) expect(declarations.has(next.replace(/\.js$/, ".d.ts")), rel + " types " + next).toBe(true);
      }
    };
    for (const rel of present) if (rel.endsWith(".js")) check(dir, rel, true);
    for (const rel of declarations) if (rel.endsWith(".d.ts")) check(dtsDir, rel, false);
  });
});
