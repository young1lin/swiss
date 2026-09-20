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

/* The completeness gate (docs/38 L10a), first of the two machine gates: every tr/tk
   literal key and every trn "other" key must have a zh entry, every zh entry must be
   reachable from a literal, and no entry may equal its key (an untranslated copy is a bug
   dressed as coverage). Counted with the real parser, not a regex: only CallExpressions
   whose callee is exactly tr/tk/trn with literal arguments count, and a dynamic first
   argument (a tk()-marked table painted through tr(x) at call time) is legal by design —
   that half stays on review. */

import * as fs from "node:fs";
import * as path from "node:path";
import ts from "typescript";
import { describe, expect, it } from "vitest";

const here = import.meta.dirname;
const srcDir = path.resolve(here, "../src");

const { listSources } = await import(new URL("../build.mjs", import.meta.url).href) as {
  listSources: () => string[];
};
const zh: Record<string, string> = (await import("../src/locales/zh.js")).default;

/* key -> "file" of its first literal sighting, so a failure names where to look. */
function collect(): Map<string, string> {
  const used = new Map<string, string>();
  for (const rel of listSources()) {
    const text = fs.readFileSync(path.join(srcDir, rel), "utf8");
    const sf = ts.createSourceFile(rel, text, ts.ScriptTarget.Latest, true);
    const visit = (node: ts.Node): void => {
      if (ts.isCallExpression(node)) {
        const name = ts.isIdentifier(node.expression) ? node.expression.text : "";
        const lit = (i: number): string | null => {
          const a = node.arguments[i];
          return a && (ts.isStringLiteral(a) || ts.isNoSubstitutionTemplateLiteral(a)) ? a.text : null;
        };
        if (name === "tr" || name === "tk") {
          const key = lit(0);
          if (key !== null && !used.has(key)) used.set(key, rel);
        } else if (name === "trn") {
          // The "other" form is the key every language needs; "one" is English-only (L3).
          const key = lit(2);
          if (key !== null && !used.has(key)) used.set(key, rel);
        }
      }
      ts.forEachChild(node, visit);
    };
    visit(sf);
  }
  return used;
}

const has = (o: Record<string, string>, k: string): boolean => Object.prototype.hasOwnProperty.call(o, k);

/* Keys whose Chinese IS their English (docs/38 L12 fallout): brand and product names,
   unit-only counts, and separator-skeleton keys. An entry may sit in the dictionary
   identical to its key only by being on this list, each with its reason — the
   value === k rule stays a real gate for everything else.
   - "MCP", "Token", "Base URL": product vocabulary, used as-is in Chinese UI copy.
   - "{label} — {error}": an em-dash skeleton; Chinese keeps the same punctuation.
   - "HTTP {n}", "{n} MB": units and codes stay Latin in Chinese technical copy.
   - "· {user} · {auth}": a middle-dot metadata skeleton between data values.
   - "· {error}": the plugins row's error tail — {error} is the host's own message.
   - "id {id}": a data prefix (the token row's desc lead) — the payload is the id.
   - "{client}  ·  /{mcp}  ·  {status}  ·  {ms}ms  ·  {when}": the traffic row's meta line — a pure data skeleton; its only words ({status}: ok/err) translate under their own keys.
   - "{name}: {error}" / "{name}: {msg}": toast skeletons — a name and an already-translated payload.
   - "{verb} → {state}": the lifecycle action note — {verb} arrives translated, {state} is the host's state word (L9).
   - "{when}  ·  {via}  ·  {client}  ·  {ms} ms  ·  {chars}" (both shapes): the call log's meta line — pure data skeleton.
   - "✗ {error}": a bare failure marker before the host's own error text.
   - "SELECT 1": example SQL in a placeholder — language-neutral.
   - "build", "/data/ws/proj": sample values in the remote sheet's placeholders — a plausible
     alias and a plausible POSIX root, both language-neutral as samples.
   - "socks5://127.0.0.1:7890": the proxy URL placeholder's sample URL — language-neutral.
   - "5433", "5432", "127.0.0.1": the rule sheet's sample ports and host — numbers and the
     loopback address, language-neutral as samples.
   - "DEPLOY_ENV=staging\nLOG_DIR=C:\\logs", "nightly-vacuum", "cmd /c backup.bat --flag value",
     "30 3 * * *", "ops, nightly": the jobs sheets' sample values — an env block, a job id, a
     command line, a cron expression, labels; all language-neutral as samples.
   - "retry.retryOn": the jobs config's own key path, shown as the field's label.
   - "CSV", "SQL", "DDL": format and language names — brands.
   - "events": the new-table sheet's sample table name — language-neutral sample.
   - " · {note}": a data frame around the server's own note, kept verbatim.
   - "id,name\n1,alice\n2,bob": the import sheet's sample CSV — data-shaped sample.
   - "TRUE", "FALSE", "null": rendered SQL/JSON vocabulary, not copy.
   - "git-mcp", "prod": the add sheets' sample ids — language-neutral samples. */
const PASSTHROUGH = new Set(["MCP", "Token", "Base URL", "{label} — {error}", "HTTP {n}", "{n} MB", "· {user} · {auth}", "· {error}", "id {id}", "{client}  ·  /{mcp}  ·  {status}  ·  {ms}ms  ·  {when}", "{name}: {error}", "{name}: {msg}", "{verb} → {state}", "{when}  ·  {via}  ·  {client}  ·  {ms} ms  ·  {chars}", "{when}  ·  {via}  ·  {ms} ms  ·  {chars}", "✗ {error}", "SELECT 1", "build", "/data/ws/proj", "socks5://127.0.0.1:7890", "5433", "5432", "127.0.0.1", "DEPLOY_ENV=staging\nLOG_DIR=C:\\logs", "nightly-vacuum", "cmd /c backup.bat --flag value", "30 3 * * *", "ops, nightly", "retry.retryOn", "CSV", "SQL", "DDL", "events", " · {note}", "id,name\n1,alice\n2,bob", "TRUE", "FALSE", "null", "git-mcp", "prod"]);

describe("i18n dictionary completeness (docs/38 L10a)", () => {
  const used = collect();

  it("every literal key has a zh entry", () => {
    expect(used.size, "the scanner found nothing - it is broken, not the tree clean").toBeGreaterThan(0);
    const missing = [...used.entries()].filter(([k]) => !has(zh, k)).map(([k, where]) => k + " (used in " + where + ")");
    expect(missing, "missing zh: …").toEqual([]);
  });

  it("no zh entry is orphaned", () => {
    const orphans = Object.keys(zh).filter((k) => !used.has(k));
    expect(orphans, "orphan zh: …").toEqual([]);
  });

  it("no zh entry forgot to translate (value === key)", () => {
    const copied = Object.entries(zh).filter(([k, v]) => v === k && !PASSTHROUGH.has(k)).map(([k]) => k);
    expect(copied, "untranslated copy: …").toEqual([]);
  });
});