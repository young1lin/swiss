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

/* The part assignment for the split locale tables (SPEC §panel.i18n). This table is the
 * single source: a top-level prefix lives in exactly one part, shell takes every prefix
 * the table does not name, and the two locales carry the same part files. zh may omit a
 * .one key (zh-CN has no "one" category) and nothing else. A spread that silently
 * overwrites a duplicate would make the barrel's key count smaller than the sum. */

import { spawnSync } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import en from "../src/locales/en.js";
import enData from "../src/locales/en/data.js";
import enDataMongo from "../src/locales/en/data-mongo.js";
import enGallery from "../src/locales/en/gallery.js";
import enLlm from "../src/locales/en/llm.js";
import enMemory from "../src/locales/en/memory.js";
import enOps from "../src/locales/en/ops.js";
import enShell from "../src/locales/en/shell.js";
import zh from "../src/locales/zh.js";
import zhData from "../src/locales/zh/data.js";
import zhDataMongo from "../src/locales/zh/data-mongo.js";
import zhGallery from "../src/locales/zh/gallery.js";
import zhLlm from "../src/locales/zh/llm.js";
import zhMemory from "../src/locales/zh/memory.js";
import zhOps from "../src/locales/zh/ops.js";
import zhShell from "../src/locales/zh/shell.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const panel = path.resolve(here, "..");

/* Brief 01's assignment, in the brief's order. `data` is every data* prefix other than
 * dataMongo, plus analysis. `ops` is the named operational prefixes. */
const PARTS = ["memory", "llm", "gallery", "data-mongo", "data", "ops", "shell"] as const;

const OPS = new Set([
  "process", "jobs", "logs", "runHistory", "remoteRuns", "remote",
  "tunnels", "tunnelSheets", "terminal", "terminalSettings", "traffic",
]);

function partFor(prefix: string): string {
  if (prefix === "memory" || prefix === "llm" || prefix === "gallery") return prefix;
  if (prefix === "dataMongo") return "data-mongo";
  if (prefix === "analysis" || /^data([A-Z]|$)/.test(prefix)) return "data";
  if (OPS.has(prefix)) return "ops";
  return "shell";
}

const enParts: Record<string, Record<string, string>> = {
  memory: enMemory,
  llm: enLlm,
  gallery: enGallery,
  "data-mongo": enDataMongo,
  data: enData,
  ops: enOps,
  shell: enShell,
};

const zhParts: Record<string, Record<string, string>> = {
  memory: zhMemory,
  llm: zhLlm,
  gallery: zhGallery,
  "data-mongo": zhDataMongo,
  data: zhData,
  ops: zhOps,
  shell: zhShell,
};

const has = (o: Record<string, string>, k: string): boolean => Object.prototype.hasOwnProperty.call(o, k);

/* Same exemption as i18n-complete.test.ts: zh never carries a .one plural. */
const oneKey = (k: string): boolean => /[.]one$/.test(k);

function partFiles(locale: string): string[] {
  return fs.readdirSync(path.join(panel, "src", "locales", locale))
    .filter((name) => name.endsWith(".ts"))
    .map((name) => name.slice(0, -3))
    .sort();
}

function prefixOf(key: string): string {
  return key.split(".")[0];
}

describe("locale parts (SPEC §panel.i18n)", () => {
  it("en and zh contain the same part files", () => {
    const names = [...PARTS].sort();
    expect(partFiles("en")).toEqual(names);
    expect(partFiles("zh")).toEqual(names);
    expect(Object.keys(enParts).sort()).toEqual(names);
    expect(Object.keys(zhParts).sort()).toEqual(names);
  });

  it("no key appears in two parts", () => {
    const failures: string[] = [];
    for (const [label, parts] of [["en", enParts], ["zh", zhParts]] as const) {
      const seen = new Map<string, string>();
      for (const [part, table] of Object.entries(parts)) {
        for (const key of Object.keys(table)) {
          const prev = seen.get(key);
          if (prev !== undefined) failures.push(label + " " + key + " in " + prev + " and " + part);
          else seen.set(key, part);
        }
      }
    }
    expect(failures).toEqual([]);
  });

  it("every key's prefix belongs to the part that holds it", () => {
    const failures: string[] = [];
    for (const [label, parts] of [["en", enParts], ["zh", zhParts]] as const) {
      for (const [part, table] of Object.entries(parts)) {
        for (const key of Object.keys(table)) {
          const want = partFor(prefixOf(key));
          if (want !== part) failures.push(label + " " + key + " is in " + part + ", rule says " + want);
        }
      }
    }
    expect(failures).toEqual([]);
  });

  it("each part's zh keys equal its en keys, apart from the .one exemption", () => {
    const failures: string[] = [];
    for (const part of PARTS) {
      const enKeys = Object.keys(enParts[part]);
      const zhKeys = new Set(Object.keys(zhParts[part]));
      for (const key of enKeys) {
        if (oneKey(key)) {
          if (has(zhParts[part], key)) failures.push(part + " zh still has " + key);
        } else if (!zhKeys.has(key)) {
          failures.push(part + " zh missing " + key);
        }
      }
      for (const key of zhKeys) {
        if (!has(enParts[part], key)) failures.push(part + " zh extra " + key);
      }
    }
    expect(failures).toEqual([]);
  });

  it("each barrel's key count equals the sum of its parts", () => {
    const sum = (parts: Record<string, Record<string, string>>): number =>
      Object.values(parts).reduce((n, table) => n + Object.keys(table).length, 0);
    expect(Object.keys(en).length).toBe(sum(enParts));
    expect(Object.keys(zh).length).toBe(sum(zhParts));
  });

  it("the historical migration refuses the split tables", () => {
    const script = path.join(panel, "scripts", "migrate-i18n-keys.mjs");
    const result = spawnSync(process.execPath, [script], { cwd: panel, encoding: "utf8" });
    expect(result.status).toBe(1);
    expect(result.stderr).toContain(
      "historical migration: the locale tables are split into locales/en/ and locales/zh/",
    );
  });
});
