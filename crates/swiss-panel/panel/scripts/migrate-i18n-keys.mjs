/*
 * The docs/38 key-normalization codemod (2026-10-30 decision): rewrites every
 * tr()/trn()/tk() English-string literal in panel/src and panel/test to a symbolic
 * key "<module>.<semanticId>", and regenerates locales/en.ts (key -> English) and
 * locales/zh.ts (key -> Chinese, values carried over from the English-keyed table).
 *
 * Run ONCE from crates/swiss-panel/panel:  node scripts/migrate-i18n-keys.mjs
 * The script is idempotent-safe to re-run only on the pre-migration tree; it exists
 * in the repo as the record of how the migration was performed. A re-run on the
 * ALREADY-migrated tree would treat every symbolic key as English copy and corrupt the
 * call sites and en.ts, so the guard below refuses to start and exits non-zero.
 */
import * as fs from "node:fs";
import * as path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import ts from "typescript";

const here = path.dirname(fileURLToPath(import.meta.url));
const panel = path.resolve(here, "..");
const srcDir = path.join(panel, "src");

/* --- the re-run guard: a tr()/tk()/trn() literal that already reads as a symbolic key
 * (lowercase module dot camelCase id) proves the migration already ran; rewriting again
 * would mint keys FROM keys and burn the dictionaries. Refuse loudly, not silently. */
for (const rel of fs.readdirSync(srcDir, { withFileTypes: true })) {
  if (!rel.isFile() || !rel.name.endsWith(".ts")) continue;
  const body = fs.readFileSync(path.join(srcDir, rel.name), "utf8");
  if (/tr\("([a-z][a-zA-Z0-9]*\.[a-z][A-Za-z0-9]*)"/.test(body) || /tk\("([a-z][a-zA-Z0-9]*\.[a-z][A-Za-z0-9]*)"/.test(body)) {
    console.error("refusing to run: " + rel.name + " already carries symbolic keys - the migration has already been performed");
    process.exit(1);
  }
}

/* --- the wire vocabulary: labels the gateway serves on /api/plugins. They reach the
 * screen through tr(variable), so the codemod cannot see them at call sites; they get
 * a dedicated wire.* namespace and the runtime maps served text -> key (i18n.ts). */
const WIRE = ["MCP", "Tunnels", "Data", "Jobs", "Process", "Terminal", "Remote", "Settings",
  "Servers", "Traffic", "Token", "SSH Connections", "Port Forwards", "Targets", "Runs",
  "Plugins", "Secrets", "System"];

const STOP = new Set(["the", "a", "an", "to", "of", "in", "for", "and", "or", "is", "it",
  "this", "that", "on", "at", "by", "with", "from", "as", "be", "are", "was", "not", "no",
  "your", "its", "has", "have", "did", "does", "do", "so", "if", "up", "out", "yet", "per", "via"]);

const cap = (w, i) => (i === 0 ? w : w.charAt(0).toUpperCase() + w.slice(1));

function slugify(text) {
  let t = text.replace(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g, (_m, n) => " " + n + " ");
  t = t.toLowerCase().replace(/[\u2019'’]/g, "").replace(/[^a-z0-9]+/g, " ").trim();
  const words = t.split(/\s+/).filter(Boolean).filter((w) => !STOP.has(w)).map((w) => (/^\d/.test(w) ? "n" + w : w));
  const pick = words.slice(0, 4);
  if (!pick.length) return "text";
  return pick.map(cap).join("");
}

const camel = (s) => s.replace(/[-_.](.)/g, (_m, c) => c.toUpperCase());
const q = (s) => JSON.stringify(s);

/* --- read the existing zh table (English keys) via the committed emit, which the last
 * npm run build refreshed - safer than eval'ing the TS source. */
const zhJsPath = path.join(panel, "../src/admin_assets/js/locales/zh.js");
const zhModule = await import(pathToFileURL(zhJsPath).href);
const oldZh = zhModule.default;

/* --- collect the calls ------------------------------------------------------------------------- */
function listFiles(dir, out) {
  for (const ent of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, ent.name);
    if (ent.isDirectory()) { if (ent.name !== "locales") listFiles(p, out); }
    else if (ent.name.endsWith(".ts")) out.push(p);
  }
}
const srcFiles = [];
listFiles(srcDir, srcFiles);
const testDir = path.join(panel, "test");
const testFiles = fs.readdirSync(testDir).filter((f) => f.endsWith(".ts")
  && f !== "i18n-complete.test.ts" && f !== "i18n-ratchet.test.ts").map((f) => path.join(testDir, f));

/* per-module english -> key; global english -> key for test resolution */
const moduleMaps = new Map(); // module -> Map(english -> key)
const globalMap = new Map();  // english -> key (first src module wins)
const takenSlugs = new Map(); // module -> Set(slug base in use)
const enEntries = [];         // { module, key, en } in discovery order
const zhMissing = [];

const enSeen = new Set(); // one entry per key - repeated literals must not mint duplicates
function pushEntry(e) { if (!enSeen.has(e.key)) { enSeen.add(e.key); enEntries.push(e); } }

function keyFor(module, english, kind) {
  if (!moduleMaps.has(module)) { moduleMaps.set(module, new Map()); takenSlugs.set(module, new Set()); }
  const map = moduleMaps.get(module);
  const cacheKey = kind + "\u0000" + english;
  if (map.has(cacheKey)) return map.get(cacheKey);
  let base = slugify(english);
  const taken = takenSlugs.get(module);
  if (taken.has(base)) {
    // disambiguate: widen to 6 significant words, then numeric suffix
    let t = english.replace(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g, (_m, n) => " " + n + " ").toLowerCase().replace(/[^a-z0-9]+/g, " ").trim();
    const words = t.split(/\s+/).filter(Boolean).filter((w) => !STOP.has(w)).map((w) => (/^\d/.test(w) ? "n" + w : w));
    base = words.slice(0, 6).map(cap).join("") || base;
    let i = 2;
    while (taken.has(base)) base = slugify(english) + String(i++);
  }
  taken.add(base);
  const key = module + "." + base;
  map.set(cacheKey, key);
  return key;
}

/* one pass over a file: record literal call args; rewrite them */
function rewriteFile(file, module, resolveGlobal) {
  const text = fs.readFileSync(file, "utf8");
  const sf = ts.createSourceFile(file, text, ts.ScriptTarget.Latest, true);
  const edits = [];
  const visit = (node) => {
    if (ts.isCallExpression(node)) {
      const name = ts.isIdentifier(node.expression) ? node.expression.text : "";
      if (name === "tr" || name === "tk") {
        const a = node.arguments[0];
        if (a !== undefined && ts.isStringLiteral(a)) {
          const english = a.text;
          let key;
          if (resolveGlobal && globalMap.has("tr\u0000" + english)) key = globalMap.get("tr\u0000" + english);
          else { key = keyFor(module, english, name === "tr" ? "tr" : "tk"); if (!resolveGlobal) globalMap.set("tr\u0000" + english, key); pushEntry({ module, key, en: english }); if (!Object.prototype.hasOwnProperty.call(oldZh, english)) zhMissing.push(key + " <- " + english); }
          edits.push([a.getStart(sf), a.getEnd(), q(key)]);
        }
      } else if (name === "trn") {
        const one = node.arguments[1];
        const other = node.arguments[2];
        if (one !== undefined && other !== undefined && ts.isStringLiteral(one) && ts.isStringLiteral(other)) {
          const cacheKey = "trn\u0000" + other.text;
          let baseKey;
          if (resolveGlobal && globalMap.has(cacheKey)) baseKey = globalMap.get(cacheKey);
          else {
            // base = the full other-form key; the pair's keys are base.one / base.other
            const otherKey = keyFor(module, other.text, "trn");
            baseKey = otherKey;
            globalMap.set(cacheKey, baseKey);
            pushEntry({ module, key: baseKey + ".one", en: one.text });
            pushEntry({ module, key: baseKey + ".other", en: other.text });
            if (!Object.prototype.hasOwnProperty.call(oldZh, other.text)) zhMissing.push(baseKey + ".other <- " + other.text);
          }
          edits.push([one.getStart(sf), one.getEnd(), q(baseKey + ".one")]);
          edits.push([other.getStart(sf), other.getEnd(), q(baseKey + ".other")]);
        }
      }
    }
    ts.forEachChild(node, visit);
  };
  visit(sf);
  if (!edits.length) return 0;
  edits.sort((x, y) => y[0] - x[0]);
  let out = text;
  for (const [s, e, rep] of edits) out = out.slice(0, s) + rep + out.slice(e);
  fs.writeFileSync(file, out, "utf8");
  return edits.length;
}

let total = 0;
const changed = [];
for (const f of srcFiles) {
  const module = camel(path.basename(f, ".ts"));
  const n = rewriteFile(f, module, false);
  if (n) { total += n; changed.push(path.basename(f) + ":" + n); }
}
for (const f of testFiles) {
  const n = rewriteFile(f, "test", true);
  if (n) { total += n; changed.push("test/" + path.basename(f) + ":" + n); }
}

/* wire namespace entries */
for (const label of WIRE) {
  const key = "wire." + camel(label.toLowerCase().replace(/[^a-z0-9]+/g, " ").trim().replace(/\s+(.)/g, (m, c) => c.toUpperCase()).replace(/^./, (c) => c.toLowerCase()));
  enEntries.push({ module: "wire", key, en: label });
}

/* --- emit en.ts and zh.ts ------------------------------------------------------------------------ */
const HEADER = (name, note) => `/*
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

/* ${note} */

`;

function renderTable(varName, entries) {
  const byModule = new Map();
  for (const e of entries) {
    if (!byModule.has(e.module)) byModule.set(e.module, []);
    byModule.get(e.module).push(e);
  }
  const parts = [];
  for (const [module, list] of byModule) {
    parts.push("  /* --- " + module + " --- */");
    for (const e of list) {
      const zh = varName === "zh" ? (Object.prototype.hasOwnProperty.call(oldZh, e.en) ? oldZh[e.en] : null) : null;
      if (varName === "zh" && /\.one$/.test(e.key)) continue; // zh never carries the one form
      const value = varName === "en" ? e.en : (zh !== null ? zh : e.en);
      parts.push("  " + q(e.key) + ": " + q(value) + ",");
    }
  }
  return "const " + varName + ": Record<string, string> = {\n" + parts.join("\n") + "\n};\n\nexport default " + varName + ";\n";
}

fs.writeFileSync(path.join(srcDir, "locales/en.ts"),
  HEADER("en", "The English dictionary (docs/38, normalized 2026-10-30): symbolic keys, module-sectioned.\n   English copy is editable here without touching call sites or any other locale.")
  + renderTable("en", enEntries), "utf8");

fs.writeFileSync(path.join(srcDir, "locales/zh.ts"),
  HEADER("zh", "The Chinese dictionary (docs/38 L3): symbolic keys, module-sectioned, values carried\n   over from the English-keyed table by the migration. A plural's entry is the .other form\n   only — Chinese has no \"one\" category under Intl.PluralRules(\"zh-CN\"), so trn() never\n   looks for one.")
  + renderTable("zh", enEntries), "utf8");

console.log("edits=" + total + " files=" + changed.length);
console.log(changed.join(" "));
console.log("zh-missing (should be 0): " + zhMissing.length + (zhMissing.length ? "\n  " + zhMissing.slice(0, 10).join("\n  ") : ""));
console.log("en entries=" + enEntries.length);
