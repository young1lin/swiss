#!/usr/bin/env node
// Sequential per-module e2e runner: each file boots and stops its own gateway on the
// dedicated e2e port, so modules stay isolated and the port is never contended.
import { spawnSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const dir = dirname(fileURLToPath(import.meta.url));
const only = process.argv.slice(2);
const files = readdirSync(dir).filter((f) => f.endsWith(".test.mjs") && (!only.length || only.some((o) => f.includes(o)))).sort();

let failed = 0;
for (const f of files) {
  process.stdout.write(`\n=== ${f} ===\n`);
  // A live file (memory-init) waits on a real model, so its budget is longer than the rest.
  const r = spawnSync(process.execPath, [join(dir, f)], { stdio: "inherit", timeout: f.includes("init") ? 1_200_000 : 300_000 });
  if (r.status !== 0) { failed++; process.stdout.write(`!!! ${f} FAILED (${r.status})\n`); }
}
process.stdout.write(failed === 0 ? "\ne2e: all modules passed\n" : `\ne2e: ${failed} module(s) failed\n`);
process.exit(failed === 0 ? 0 : 1);
