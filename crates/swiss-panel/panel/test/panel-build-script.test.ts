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

/* build.mjs's own contract (docs/36 D2), driven as a real subprocess the way deploy.ps1 and
   CI will run it: non-erasable syntax fails naming file and line, --check refuses stale
   emissions and orphans without writing, and an unchanged tree keeps its mtimes (the debug
   build serves from disk per request and must not look fresh when nothing changed). */

import { describe, expect, it } from "vitest";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, symlinkSync, writeFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";

const buildSrc = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "build.mjs"), "utf8");

/* A scratch root with build.mjs at root/inner/build.mjs, so its outDir resolves inside the
   root (build.mjs emits to ../src/admin_assets/js relative to itself). A junction to the
   real node_modules rides along: build.mjs imports typescript, and ESM resolution never
   walks up out of the temp tree to find it. */
function scratch(): { inner: string; src: string; out: string } {
  const root = mkdtempSync(join(tmpdir(), "panel-build-"));
  const inner = join(root, "inner");
  mkdirSync(inner);
  copyFileSync(join(dirname(fileURLToPath(import.meta.url)), "..", "build.mjs"), join(inner, "build.mjs"));
  symlinkSync(join(dirname(fileURLToPath(import.meta.url)), "..", "node_modules"), join(inner, "node_modules"), "junction");
  const src = join(inner, "src");
  const out = join(root, "src", "admin_assets", "js");
  mkdirSync(src, { recursive: true });
  return { inner, src, out };
}

function run(inner: string, arg?: string) {
  const args = [join(inner, "build.mjs")];
  if (arg) args.push(arg);
  return spawnSync(process.execPath, args, { encoding: "utf8" });
}

describe("panel build script (docs/36 D2)", () => {
  it("non-erasable syntax fails the build naming the file and line", () => {
    const { inner, src } = scratch();
    try {
      writeFileSync(join(src, "bad.ts"), "enum Color { Red }\nvar x = 1;\n");
      const r = run(inner);
      expect(r.status, "enum must not build").not.toBe(0);
      expect(r.stderr + r.stdout).toContain("bad.ts:1");
    } finally {
      rmSync(dirname(inner), { recursive: true, force: true });
    }
  });

  it("--check reports an orphan .js without writing anything", () => {
    const { inner, src, out } = scratch();
    try {
      writeFileSync(join(src, "ok.ts"), "var one = 1;\n");
      expect(run(inner).status).toBe(0);
      const orphan = join(out, "leftover.js");
      writeFileSync(orphan, "var gone = 1;\n");
      const before = statSync(join(out, "ok.js")).mtimeMs;
      const r = run(inner, "--check");
      expect(r.status, "an orphan must fail --check").not.toBe(0);
      expect(r.stdout).toContain("leftover.js");
      expect(statSync(join(out, "ok.js")).mtimeMs, "--check never writes").toBe(before);
    } finally {
      rmSync(dirname(inner), { recursive: true, force: true });
    }
  });

  it("--check reports a stale emission", () => {
    const { inner, src, out } = scratch();
    try {
      writeFileSync(join(src, "ok.ts"), "var one = 1;\n");
      expect(run(inner).status).toBe(0);
      writeFileSync(join(src, "ok.ts"), "var one = 2;\n");
      const r = run(inner, "--check");
      expect(r.status).not.toBe(0);
      expect(r.stdout).toContain("stale");
      expect(readFileSync(join(out, "ok.js"), "utf8"), "--check never writes").toBe("var one = 1;\n");
    } finally {
      rmSync(dirname(inner), { recursive: true, force: true });
    }
  });

  it("an unchanged tree is not rewritten (mtime is stable)", () => {
    const { inner, src, out } = scratch();
    try {
      writeFileSync(join(src, "ok.ts"), "var one = 1;\n");
      expect(run(inner).status).toBe(0);
      const before = statSync(join(out, "ok.js")).mtimeMs;
      expect(run(inner).status).toBe(0);
      expect(statSync(join(out, "ok.js")).mtimeMs).toBe(before);
    } finally {
      rmSync(dirname(inner), { recursive: true, force: true });
    }
  });

  it("emission preserves bytes for type-free sources (the T0 identity)", () => {
    // The property the whole migration rides on: a source with no type syntax emits
    // byte-identical output. If ts-blank-space ever normalizes anything, this fails first.
    const { inner, src, out } = scratch();
    try {
      const body = '/* keep */ var a = { q: "x" }; // trailing stays\nfunction f(v) { return v; }\n';
      writeFileSync(join(src, "plain.ts"), body);
      expect(run(inner).status).toBe(0);
      expect(readFileSync(join(out, "plain.js"), "utf8")).toBe(body);
    } finally {
      rmSync(dirname(inner), { recursive: true, force: true });
    }
  });
});
