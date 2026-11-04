/* Regenerates the panel's bus codecs (SPEC §bus.codegen).
 *
 * `npm run proto` writes src/bus/pb. `npm run proto:check` writes a scratch
 * directory and fails if it differs, which is what `npm run check` runs.
 * The plugin path is absolute because buf refuses a relative executable.
 */

import { spawnSync } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { arch, platform, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

/* Files that differ, or that exist on only one side. An empty list is a match. */
export function protoDiff(leftDir, rightDir) {
  const walk = (dir, rel, into) => {
    for (const name of readdirSync(dir, { withFileTypes: true })) {
      const next = rel ? rel + "/" + name.name : name.name;
      if (name.isDirectory()) walk(join(dir, name.name), next, into);
      else into.push(next);
    }
  };
  const left = [];
  const right = [];
  walk(leftDir, "", left);
  walk(rightDir, "", right);
  left.sort();
  right.sort();
  const problems = [];
  if (left.join("\n") !== right.join("\n")) problems.push("the generated file list differs");
  for (const rel of left) {
    if (!right.includes(rel)) continue;
    const a = readFileSync(join(leftDir, rel));
    const b = readFileSync(join(rightDir, rel));
    if (Buffer.compare(a, b) !== 0) problems.push(rel);
  }
  return problems;
}

function main() {
  const check = process.argv.includes("--check");
  const pluginName = platform() === "win32" ? "protoc-gen-es.cmd" : "node_modules/@bufbuild/protoc-gen-es/bin/protoc-gen-es";
  const bufName = {
    "win32 x64": "@bufbuild/buf-win32-x64/bin/buf.exe",
    "win32 arm64": "@bufbuild/buf-win32-arm64/bin/buf.exe",
    "linux x64": "@bufbuild/buf-linux-x64/bin/buf",
    "linux arm64": "@bufbuild/buf-linux-aarch64/bin/buf",
    "darwin x64": "@bufbuild/buf-darwin-x64/bin/buf",
    "darwin arm64": "@bufbuild/buf-darwin-arm64/bin/buf",
  }[platform() + " " + arch()];
  if (!bufName) {
    process.stderr.write("no buf binary for " + platform() + " " + arch() + "\n");
    process.exit(1);
  }
  const plugin = resolve(here, pluginName).replaceAll("\\", "/");
  const buf = resolve(here, "node_modules", bufName);
  const out = check ? mkdtempSync(join(tmpdir(), "swiss-bus-proto-")) : resolve(here, "src/bus/pb");
  const scratch = check ? mkdtempSync(join(tmpdir(), "swiss-bus-proto-tpl-")) : here;
  const template = readFileSync(join(here, "buf.gen.yaml"), "utf8")
    .replace("local: protoc-gen-es.cmd", "local: " + plugin)
    .replace("out: src/bus/pb", "out: " + out.replaceAll("\\", "/"));
  const templatePath = join(scratch, "buf.gen.run.yaml");
  writeFileSync(templatePath, template);
  const run = spawnSync(buf, ["generate", "--template", templatePath], { cwd: here, encoding: "utf8" });
  rmSync(check ? scratch : templatePath, { recursive: check, force: true });
  if (run.status !== 0) {
    process.stderr.write(run.stdout ?? "");
    process.stderr.write(run.stderr ?? "");
    process.exit(run.status ?? 1);
  }
  if (check) {
    const problems = protoDiff(resolve(here, "src/bus/pb"), out);
    rmSync(out, { recursive: true, force: true });
    if (problems.length) {
      process.stderr.write("stale bus codecs (" + problems.join(", ") + "); run: npm run proto\n");
      process.exit(1);
    }
  }
  process.stdout.write("proto ok\n");
}

const invoked = process.argv[1] ? resolve(process.argv[1]) : "";
if (invoked === fileURLToPath(import.meta.url)) main();
