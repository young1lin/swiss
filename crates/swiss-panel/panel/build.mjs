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

/* Emits the served panel tree from the TypeScript sources (docs/36 D2).
 *
 * panel/src (TypeScript) -types-blanked-in-place-> ../src/admin_assets/js (JavaScript)
 *
 * ts-blank-space replaces type syntax with spaces and touches NOTHING else: comments,
 * quotes, line breaks and layout survive byte for byte, so the browser's module graph keeps
 * the paths, the line numbers and the bug-record comments the sources carry. That property
 * is the whole reason this tool was chosen over tsc's printer, which reindents, splits
 * statements and drops comments (docs/36 §0.1 measured it).
 *
 * Contract (pinned by test/panel-emit.test.ts and test/panel-build-script.test.ts):
 * - walks every .ts under src/, skips the .d.ts files (pure types never emit), and
 *   mirrors every other path 1:1;
 * - non-erasable syntax (enum, namespace, parameter properties, decorators) fails the run
 *   naming the file and line - tsconfig's erasableSyntaxOnly is the same gate at check time;
 * - writes only when the content differs, so an unchanged tree keeps its mtimes (the debug
 *   build serves from disk per request and must not look fresh when nothing changed);
 * - --check compares without writing and also fails on orphan .js files under js/ that have
 *   no .ts source (a deleted module must be deleted in both trees by hand);
 * - never touches js/vendor/ and never deletes anything;
 * - --watch re-emits one file per change for the save -> emit -> refresh dev loop.
 */

import { watch } from "node:fs";
import { mkdirSync, readdirSync, readFileSync, realpathSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";
import { blankSourceFile } from "ts-blank-space";

const here = dirname(fileURLToPath(import.meta.url));
const srcDir = join(here, "src");
// The committed, served tree: rust-embed embeds it, cargo build never runs node (docs/36 D3).
const outDir = join(here, "..", "src", "admin_assets", "js");

/* All non-declaration sources, as "/"-separated paths relative to src/ (stable across
   platforms, and the same shape as the emitted path below js/). */
export function listSources() {
  const out = [];
  const walk = (rel) => {
    for (const f of readdirSync(join(srcDir, rel), { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const p = rel ? rel + "/" + f.name : f.name;
      if (f.isDirectory()) walk(p);
      else if (f.name.endsWith(".ts") && !f.name.endsWith(".d.ts")) out.push(p);
    }
  };
  walk("");
  return out;
}

/* All emitted files under js/ that are ours, same path shape. vendor/ is hand-placed and
   exempt: xterm and cronstrue ship verbatim in whatever dialect they came in. */
export function listEmitted() {
  const out = [];
  const walk = (rel) => {
    for (const f of readdirSync(join(outDir, rel), { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name))) {
      const p = rel ? rel + "/" + f.name : f.name;
      if (p === "vendor") continue;
      if (f.isDirectory()) walk(p);
      else if (f.name.endsWith(".js")) out.push(p);
    }
  };
  walk("");
  return out;
}

/* One source -> emitted text. Non-erasable syntax is a build failure, not a warning: the
   served tree would otherwise silently carry runtime syntax the panel never agreed to. The
   source file is parsed here (bring-your-own-AST) so the error callback can name the line:
   the nodes the callback receives do not carry a usable getSourceFile(). */
export function emitOne(rel) {
  const text = readFileSync(join(srcDir, rel), "utf8");
  const sf = ts.createSourceFile(rel, text, ts.ScriptTarget.Latest, true);
  return blankSourceFile(sf, (node) => {
    const pos = sf.getLineAndCharacterOfPosition(node.getStart(sf));
    throw new Error(rel + ":" + (pos.line + 1) + ":" + (pos.character + 1) +
      " - non-erasable syntax (" + ts.SyntaxKind[node.kind] +
      "): the panel must stay erasable-only (docs/36 D4)");
  });
}

/* Emit every source, writing only changed files. Returns the per-file outcome for callers
   (the tests re-emit in memory through emitOne, so this stays report-shaped, not print-shaped). */
export function emitAll() {
  const written = [];
  const unchanged = [];
  for (const rel of listSources()) {
    const jsPath = join(outDir, rel.replace(/\.ts$/, ".js"));
    const content = emitOne(rel);
    let prev = null;
    try { prev = readFileSync(jsPath, "utf8"); } catch { /* first emission of a new module */ }
    if (prev === content) { unchanged.push(rel); continue; }
    mkdirSync(dirname(jsPath), { recursive: true });
    writeFileSync(jsPath, content, { flag: "w" }); // LF in, LF out: nothing is transformed
    written.push(rel);
  }
  return { written, unchanged };
}

/* --check: freshness plus orphan detection, no writes. Returns the problems found. */
export function checkAll() {
  const stale = [];
  const orphans = [];
  const sources = new Set(listSources().map((p) => p.replace(/\.ts$/, ".js")));
  for (const rel of listSources()) {
    const jsPath = join(outDir, rel.replace(/\.ts$/, ".js"));
    let prev = null;
    try { prev = readFileSync(jsPath, "utf8"); } catch { prev = null; }
    if (prev === null) stale.push(rel + " -> missing " + rel.replace(/\.ts$/, ".js"));
    else if (prev !== emitOne(rel)) stale.push(rel + " -> emitted .js is stale, run: npm run build");
  }
  for (const js of listEmitted()) {
    if (!sources.has(js)) orphans.push(js + " has no panel/src counterpart");
  }
  return { stale, orphans };
}

/* CLI only when run directly: the emit tests import the functions above, and a module import
   that silently re-emitted the tree would make every freshness check self-fulfilling. */
const isMain = (() => {
  try {
    return process.argv[1] && realpathSync(fileURLToPath(import.meta.url)) === realpathSync(process.argv[1]);
  } catch { return false; }
})();

const args = process.argv.slice(2);
if (isMain && args.includes("--check")) {
  const { stale, orphans } = checkAll();
  for (const s of stale) console.log("STALE " + s);
  for (const o of orphans) console.log("ORPHAN " + o);
  if (stale.length || orphans.length) process.exit(1);
  console.log("panel emit fresh: " + listSources().length + " sources, no orphans");
} else if (isMain && args.includes("--watch")) {
  // The dev loop (docs/36 D13): save -> emit that one file -> refresh the debug build.
  const pending = new Map(); // rel -> timer
  const emitIfSource = (rel) => {
    if (!rel.endsWith(".ts") || rel.endsWith(".d.ts")) return;
    try {
      const content = emitOne(rel);
      const jsPath = join(outDir, rel.replace(/\.ts$/, ".js"));
      let prev = null;
      try { prev = readFileSync(jsPath, "utf8"); } catch { prev = null; }
      if (prev !== content) {
        mkdirSync(dirname(jsPath), { recursive: true });
        writeFileSync(jsPath, content, { flag: "w" });
        console.log("emit " + rel);
      }
    } catch (e) { console.error(String(e)); }
  };
  watch(srcDir, { recursive: true }, (_event, filename) => {
    if (!filename) return;
    const rel = String(filename).replaceAll("\\", "/");
    clearTimeout(pending.get(rel)); // editors fire several events per save; emit once settled
    pending.set(rel, setTimeout(() => { pending.delete(rel); emitIfSource(rel); }, 50));
  });
  console.log("watching " + srcDir + " -> " + outDir);
} else if (isMain) {
  const { written, unchanged } = emitAll();
  for (const w of written) console.log("emit " + w);
  console.log("panel emit: " + written.length + " written, " + unchanged.length + " unchanged");
}
