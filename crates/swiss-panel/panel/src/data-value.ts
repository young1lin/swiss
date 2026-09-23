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

import { $ } from "./util.js";
import { fill, h } from "./h.js";
import type { HChild } from "./h.js";
import { locale, tr } from "./i18n.js";
import { closeSheet } from "./ui/sheet.js";

/* --- value viewer sheet (docs/22 W5.3) ----------------------------------------------------------- */
/* A cell is a 30px row; its content is not. "View value\u2026" opens the full value in a
   read-only sheet — the same surface the editor lives in, minus every write affordance
   (editing stays in the editor sheet; this one only looks). cloudbeaver registers value
   presentations by MIME (TextValuePresentationBootstrap); the equivalent here is one pure
   kind picker over the value itself, because the wire already says what the value is:
   - the \\x hex form (both adapters' binary ride, docs/22 W4b) opens as a hex dump,
   - an http(s) text opens as itself plus the link,
   - parseable JSON opens as a details-folded tree,
   - everything else opens as full text.
   The column type deliberately does not sway the choice: a MySQL BLOB that is valid UTF-8
   rides the wire as text (mysql.rs), and its honest presentation IS the text. */

/** The hex cap: the first 512 bytes of a binary value, then the panel's standing
 *  truncation word. The WIRE stays uncapped on purpose — the whole hex string is the
 *  value's exact address (the keyless-table md5 digests the decoded bytes, W4b) — so the
 *  sheet caps its own view, never the value it was handed. */
const DB_VALUE_HEX_MAX = 512;

function dbValueJsonLike(s: unknown): boolean {
  const t = String(s == null ? "" : s).trim();
  return t.startsWith("{") || t.startsWith("[");
}

/** Which of the four presentations a value opens as. Pure; the sheet is a thin wrapper. */
function dbValueKind(value: unknown): "hex" | "url" | "json" | "text" {
  if (typeof value === "string") {
    if (/^\\x[0-9a-fA-F]*$/.test(value)) return "hex";
    if (/^https?:\/\//i.test(value)) return "url";
    if (dbValueJsonLike(value)) {
      try { JSON.parse(value); return "json"; } catch (e) { /* not JSON — plain text */ }
    }
  } else if (value !== null && value !== undefined && typeof value === "object") {
    return "json"; // a JSON column the adapter already parsed
  }
  return "text";
}

/** The first maxBytes of the \\x wire form as a spaced hex dump, 16 bytes per line, with
 *  the real byte count (hex chars / 2, never the character count) and the truncated flag
 *  that reuses the redis value view's word ("\u00b7 truncated"). Pure. */
function dbHexPreview(value: string, maxBytes: number): { text: string; bytes: number; truncated: boolean } {
  const hex = String(value).replace(/^\\x/, "");
  const bytes = Math.floor(hex.length / 2);
  const truncated = bytes > maxBytes;
  const show = truncated ? hex.slice(0, maxBytes * 2) : hex;
  const pairs = show.match(/.{2}/g) || [];
  const lines: string[] = [];
  for (let i = 0; i < pairs.length; i += 16) lines.push(pairs.slice(i, i + 16).join(" "));
  return { text: lines.join("\n"), bytes: bytes, truncated: truncated };
}

/** One JSON node as a node tree (docs/37 R5). Containers fold behind <details> — the root
 *  open, everything nested closed — so a deep document opens to its top level and expands on
 *  demand. Object keys are identity (text face); values are values you would copy (mono).
 *  Every remote string lands as a TEXT node: a document full of tags stays inert text with
 *  no esc() anywhere. Pure in its inputs (builds nodes only, touches nothing live). */
/* The node builder takes unknown (a parsed JSON value is exactly that) and narrows by
   runtime check; the one Record downcast sits behind typeof-object + not-an-array, the
   narrowest honest shape an indexable object has. */
function dbJsonNode(v: unknown, isOpen: boolean): HChild {
  if (v === null || v === undefined) return h("span", { class: "db-val-v db-null" }, tr("dataValue.null"));
  if (typeof v !== "object") return h("span", { class: "db-val-v" }, JSON.stringify(v));
  if (Array.isArray(v)) {
    return h("details", { class: "db-val-node", open: isOpen },
      h("summary", null, v.length ? "[ " + v.length + " ]" : "[ ]"),
      ...v.map((x: unknown): HChild => { return h("div", { class: "db-val-row" }, dbJsonNode(x, false)); }));
  }
  const obj = v as Record<string, unknown>;
  const keys = Object.keys(obj);
  const summary = keys.length ? "{ " + keys.length + " }" : "{ }";
  const inner: HChild[] = [];
  keys.forEach((k: string): void => {
    inner.push(h("div", { class: "db-val-row" },
      h("span", { class: "db-val-k" }, k),
      dbJsonNode(obj[k], false)));
  });
  return h("details", { class: "db-val-node", open: isOpen },
    h("summary", null, summary),
    ...inner);
}

function dbJsonTreeNodes(v: unknown): HChild { return dbJsonNode(v, true); }

/** Open the read-only viewer. One primary action (Close); Escape and the backdrop close
 *  too. `where` is the caption the caller knows (table for grid cells, "SQL result" for
 *  console results). */
function dbOpenValueSheet(column: string, value: unknown, where: string | null | undefined): void {
  const kind = dbValueKind(value);
  let body: HChild;
  if (kind === "json") {
    const parsed = typeof value === "object" ? value : JSON.parse(value as string);
    body = h("div", { class: "db-val-tree" }, dbJsonTreeNodes(parsed));
  } else if (kind === "hex") {
    const p = dbHexPreview(value as string, DB_VALUE_HEX_MAX);
    body = h("div", null,
      h("div", { class: "db-val-meta" },
        p.truncated
          ? tr("dataValue.nBytesTruncated", { n: p.bytes.toLocaleString(locale()) })
          : tr("dataValue.nBytes", { n: p.bytes.toLocaleString(locale()) })),
      h("pre", { class: "db-val-pre" }, p.text));
  } else if (kind === "url") {
    const s = value as string;
    body = h("pre", { class: "db-val-pre" },
      h("a", { class: "db-link", href: s, target: "_blank", rel: "noopener noreferrer" }, s));
  } else {
    body = h("pre", { class: "db-val-pre" }, String(value));
  }
  // docs/37 R5: node sheet, painted AFTER the host is unhidden (first paint never lands in
  // a hidden box); per-open button wiring stays, per the sheet idiom.
  $("sheet").hidden = false;
  fill($("sheet"),
    h("div", { class: "sheet", role: "dialog", aria: { modal: "true", label: tr("dataValue.viewValue") } },
      h("div", { class: "sheet-head" },
        h("div", { class: "db-cell-head" },
          h("h2", null, column),
          h("span", { class: "db-cell-where" }, where || ""))),
      h("div", { class: "sheet-body" }, body),
      h("div", { class: "sheet-foot" }, h("span", { class: "grow" }),
        h("button", { class: "btn primary", id: "dbValClose" }, tr("dataValue.close")))));
  const onKey = (e: KeyboardEvent): void => {
    if (e.key === "Escape") { closeValueSheet(); }
  };
  const closeValueSheet = (): void => {
    document.removeEventListener("keydown", onKey);
    closeSheet();
  };
  $("dbValClose").onclick = closeValueSheet;
  document.addEventListener("keydown", onKey);
  $("sheet").onclick = (e) => { if (e.target === $("sheet")) closeValueSheet(); };
}

export { DB_VALUE_HEX_MAX, dbHexPreview, dbJsonNode, dbJsonTreeNodes, dbOpenValueSheet, dbValueKind };
