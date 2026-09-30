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

/* The SPEC §panel.i18n dictionary-completeness gate, normalized (2026-09-20): keys are
 * symbolic ("<module>.<semanticId>"), English copy lives in en.ts, every locale is its
 * own table over the same keys, and the wire vocabulary (labels the gateway serves as
 * English text on /api/plugins) is mapped at runtime by wireLabel(). Four directions
 * stay honest:
 *   - every literal key at a tr/trn/tk call site exists in EVERY locale table;
 *   - no locale carries a key nothing uses (the wire.* keys count as used);
 *   - the served wire labels all map to keys present in every locale;
 *   - zh never carries a plural ".one" form (zh-CN has no "one" category).
 * A new string ships with its call-site key in every table, one change. Adding a
 * language = one new table + it joins LOCALES below; the gates do the rest. The
 * mechanism's own unit test (i18n.test.ts) is exempt: it deliberately probes keys that
 * do not exist anywhere. */
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";
import { describe, expect, it } from "vitest";
import zh from "../src/locales/zh.js";
import en from "../src/locales/en.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const panel = path.resolve(here, "..");
const LOCALES: Record<string, Record<string, string>> = { en, zh };

const has = (o: Record<string, string>, k: string): boolean => Object.prototype.hasOwnProperty.call(o, k);

/* zh-CN never selects the "one" plural category (Intl.PluralRules), so its table carries
 * no .one keys by design - the missing-entry check must not demand them. */
const exempt = (locale: string, k: string): boolean => locale === "zh" && /[.]one$/.test(k);

/* The labels the gateway serves as English text (SPEC §panel.nav descriptors), mapped to
 * wire.* keys by wireLabel() in i18n.ts. A new page's label joins this list, the
 * WIRE_LABELS table there, and every locale table - one change. */
const WIRE_KEYS = [
  "wire.mcp", "wire.tunnels", "wire.data", "wire.jobs", "wire.process", "wire.terminal",
  "wire.remote", "wire.settings", "wire.servers", "wire.traffic", "wire.token",
  "wire.sshConnections", "wire.portForwards", "wire.targets", "wire.runs",
  "wire.plugins", "wire.secrets", "wire.system",
];

/* Every literal key at a tr()/trn()/tk() call site, collected from the TS sources with
 * the compiler (regexes mis-count closers; the AST never does). A key argument may be a
 * plain literal or a conditional over literals (tr(flag ? "k.one" : "k.two")) - both forms
 * contribute their literals; anything dynamic contributes nothing. */
function argKeys(arg: ts.Expression | undefined, out: Set<string>): void {
  if (arg === undefined) return;
  if (ts.isStringLiteral(arg)) { out.add(arg.text); return; }
  if (ts.isParenthesizedExpression(arg)) { argKeys(arg.expression, out); return; }
  /* A conditional over literals: collect the branches, never the condition - a comparison
   * inside the condition (form === "one") is code, not a key. */
  if (ts.isConditionalExpression(arg)) {
    argKeys(arg.whenTrue, out);
    argKeys(arg.whenFalse, out);
  }
}

function literalKeys(file: string, out: Set<string>): void {
  const sf = ts.createSourceFile(file, fs.readFileSync(file, "utf8"), ts.ScriptTarget.Latest, true);
  const visit = (node: ts.Node): void => {
    if (ts.isCallExpression(node)) {
      const name = ts.isIdentifier(node.expression) ? node.expression.text : "";
      if (name === "tr" || name === "tk") {
        argKeys(node.arguments[0], out);
      } else if (name === "trn") {
        argKeys(node.arguments[1], out);
        argKeys(node.arguments[2], out);
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
}

function collect(): Set<string> {
  const out = new Set<string>();
  const walkSrc = (rel: string): void => {
    for (const ent of fs.readdirSync(path.join(panel, "src", rel), { withFileTypes: true })) {
      const child = rel ? rel + "/" + ent.name : ent.name;
      if (ent.isDirectory()) { if (ent.name !== "locales") walkSrc(child); }
      else if (ent.isFile() && ent.name.endsWith(".ts") && !ent.name.endsWith(".d.ts")) literalKeys(path.join(panel, "src", child), out);
    }
  };
  walkSrc("");
  for (const ent of fs.readdirSync(path.join(panel, "test"))) {
    if (ent.endsWith(".ts") && !ent.startsWith("i18n-") && ent !== "i18n.test.ts") literalKeys(path.join(panel, "test", ent), out);
  }
  return out;
}

describe("i18n dictionary completeness (SPEC §panel.i18n, normalized keys)", () => {
  const used = collect();

  it("the scanner sees the tree (a clean pass must not be a blind pass)", () => {
    expect(used.size).toBeGreaterThan(900);
  });

  it("every literal key has an entry in every locale", () => {
    const failures: string[] = [];
    for (const [name, table] of Object.entries(LOCALES)) {
      for (const k of used) if (!exempt(name, k) && !has(table, k)) failures.push(name + " missing " + k);
    }
    expect(failures, "missing entries - ship the key in every locale table").toEqual([]);
  });

  it("no locale carries an orphan key", () => {
    const usedAll = new Set([...used, ...WIRE_KEYS]);
    const orphans: string[] = [];
    for (const [name, table] of Object.entries(LOCALES)) {
      for (const k of Object.keys(table)) if (!usedAll.has(k)) orphans.push(name + " orphan " + k);
    }
    expect(orphans, "orphan keys - remove them with the string that left").toEqual([]);
  });

  it("the wire labels map to keys with entries in every locale", () => {
    const failures: string[] = [];
    for (const k of WIRE_KEYS) {
      for (const [name, table] of Object.entries(LOCALES)) if (!has(table, k)) failures.push(name + " missing " + k);
    }
    expect(failures, "a served label would render raw").toEqual([]);
  });

  it("zh plural entries never carry a .one form", () => {
    const ones = Object.keys(zh).filter((k) => /[.]one$/.test(k));
    expect(ones, "zh-CN has no one category - the entry would be dead weight").toEqual([]);
  });
});
